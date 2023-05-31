use glob::glob;
use lib_ruby_parser::{
    nodes, traverse::visitor::Visitor, Node, Parser, ParserOptions,
};
use line_col::LineColLookup;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub module_nesting: Vec<String>,
    pub location: Range,
}

impl Reference {
    fn possible_fully_qualified_constants(&self) -> Vec<String> {
        self.module_nesting
            .iter()
            .map(|nesting| format!("{}::{}", nesting, self.name))
            .collect()
    }
}

pub struct ParsedReference {
    pub name: String,
    pub module_nesting: Vec<String>,
    pub location: Location,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedDefinition {
    pub fully_qualified_name: String,
    pub location: Location,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub begin: usize,
    pub end: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LocationRange {
    pub start: Location,
    pub end: Location,
}

struct ReferenceCollector {
    pub references: Vec<ParsedReference>,
    pub definitions: Vec<ParsedDefinition>,
    pub current_namespaces: Vec<String>,
}

#[derive(Debug)]
enum ParseError {
    Metaprogramming,
    // Add more variants as needed for different error cases
}

fn fetch_const_name(node: &nodes::Node) -> Result<String, ParseError> {
    match node {
        Node::Const(const_node) => Ok(fetch_const_const_name(const_node)?),
        Node::Cbase(_) => Ok(String::from("")),
        Node::Send(_) => Err(ParseError::Metaprogramming),
        Node::Lvar(_) => Err(ParseError::Metaprogramming),
        Node::Ivar(_) => Err(ParseError::Metaprogramming),
        Node::Self_(_) => Err(ParseError::Metaprogramming),
        node => {
            dbg!(node);
            panic!(
                "Cannot handle other node in get_constant_node_name: {:?}",
                node
            )
        }
    }
}

fn fetch_const_const_name(node: &nodes::Const) -> Result<String, ParseError> {
    match &node.scope {
        Some(s) => {
            let parent_namespace = fetch_const_name(s)?;
            Ok(format!("{}::{}", parent_namespace, node.name))
        }
        None => Ok(node.name.to_owned()),
    }
}

// TODO: Combine with fetch_const_const_name
fn fetch_casgn_name(node: &nodes::Casgn) -> Result<String, ParseError> {
    match &node.scope {
        Some(s) => {
            let parent_namespace = fetch_const_name(s)?;
            Ok(format!("{}::{}", parent_namespace, node.name))
        }
        None => Ok(node.name.to_owned()),
    }
}

impl Visitor for ReferenceCollector {
    fn on_class(&mut self, node: &nodes::Class) {
        // We're not collecting definitions, so no need to visit the class definition
        // self.visit(&node.name);
        let namespace_result = fetch_const_name(&node.name);
        // For now, we simply exit and stop traversing if we encounter an error when fetching the constant name of a class
        // We can iterate on this if this is different than the packwerk implementation
        if namespace_result.is_err() {
            return;
        }

        let namespace = namespace_result.unwrap();

        if let Some(inner) = node.superclass.as_ref() {
            self.visit(inner);
        }

        let mut name_components = self.current_namespaces.clone();
        name_components.push(namespace.to_owned());
        let fully_qualified_name = name_components.join("::");

        self.definitions.push(ParsedDefinition {
            fully_qualified_name,
            location: Location {
                begin: node.expression_l.begin,
                end: node.expression_l.end,
            },
        });

        // Note – is there a way to use lifetime specifiers to get rid of this and
        // just keep current namespaces as a vector of string references or something else
        // more efficient?
        self.current_namespaces.push(namespace);

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        self.current_namespaces.pop();
    }

    fn on_casgn(&mut self, node: &nodes::Casgn) {
        let name_result = fetch_casgn_name(node);
        if name_result.is_err() {
            return;
        }

        let name = name_result.unwrap();

        let mut name_components: Vec<String> = self.current_namespaces.clone();
        name_components.push(name);
        let fully_qualified_name = name_components.join("::");

        self.definitions.push(ParsedDefinition {
            fully_qualified_name,
            location: Location {
                begin: node.expression_l.begin,
                end: node.expression_l.end,
            },
        });
    }

    // TODO: extract the common stuff from on_class
    fn on_module(&mut self, node: &nodes::Module) {
        let namespace = fetch_const_name(&node.name)
            .expect("We expect no parse errors in class/module definitions");
        self.current_namespaces.push(namespace);

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        self.current_namespaces.pop();
    }

    fn on_const(&mut self, node: &nodes::Const) {
        if let Ok(name) = fetch_const_const_name(node) {
            self.references.push(ParsedReference {
                name,
                module_nesting: calculate_module_nesting(
                    &self.current_namespaces,
                ),
                location: Location {
                    begin: node.expression_l.begin,
                    end: node.expression_l.end,
                },
            })
        }
    }
}

// This function takes a list (`namespace_nesting`) that represents
// the level of class and module nesting at a given location in code
// and outputs the value of `Module.nesting` at that location.
// This function may have bugs! Please provide your feedback.
// I hope to iterate on it to produce an accurate-to-spec implementation
// of `Module.nesting` given the current namespace. Some bugs may involve
// improving on how the input `namespace_nesting` is generated by the
// AST visitor.
//
// # Example:
// class Foo
//   module Bar
//     class Baz
//       puts Module.nesting.inspect
//     end
//   end
// end
// # inputs: ['Foo', 'Bar', 'Baz']
// # outputs: ['Foo::Bar::Baz', 'Foo::Bar', 'Foo']
fn calculate_module_nesting(namespace_nesting: &[String]) -> Vec<String> {
    let mut nesting = Vec::new();
    let mut previous = String::from("");
    namespace_nesting.iter().for_each(|namespace| {
        let new_nesting: String = if previous.is_empty() {
            namespace.to_owned()
        } else {
            format!("{}::{}", previous, namespace)
        };

        previous = new_nesting.to_owned();
        nesting.insert(0, new_nesting);
    });

    nesting
}

pub fn get_references(absolute_root: &Path) -> Vec<Reference> {
    // Later this can come from config
    let pattern = absolute_root.join("packs/**/*.rb");

    glob(pattern.to_str().unwrap())
        .expect("Failed to read glob pattern")
        .par_bridge() // Parallel iterator
        .flat_map(|entry| match entry {
            Ok(path) => extract_from_path(&path),
            Err(e) => {
                println!("{:?}", e);
                panic!("blah");
            }
        })
        .collect()
}

pub(crate) fn extract_from_path(path: &PathBuf) -> Vec<Reference> {
    let contents = fs::read_to_string(path).unwrap_or_else(|_| {
        panic!("Failed to read contents of {}", path.to_string_lossy())
    });

    extract_from_contents(contents)
}

fn extract_from_contents(contents: String) -> Vec<Reference> {
    let options = ParserOptions {
        buffer_name: "".to_string(),
        ..Default::default()
    };

    let lookup = LineColLookup::new(&contents);
    let parser = Parser::new(contents.clone(), options);
    let _ret = parser.do_parse();

    let ast_option: Option<Box<Node>> = _ret.ast;

    let ast = match ast_option {
        Some(some_ast) => some_ast,
        None => return vec![],
    };

    // .unwrap_or_else(|| panic!("No AST found for {}!", &path.display()));
    let mut collector = ReferenceCollector {
        references: vec![],
        current_namespaces: vec![],
        definitions: vec![],
    };

    collector.visit(&ast);
    let definition_iter = collector
        .definitions
        .iter()
        .map(|d| &d.fully_qualified_name);
    let def_set: HashSet<&String> = definition_iter.collect();

    collector
        .references
        .into_iter()
        .map(|parsed_reference| {
            let (start_row, start_col) =
                lookup.get(parsed_reference.location.begin);
            let (end_row, end_col) = lookup.get(parsed_reference.location.end);

            Reference {
                name: parsed_reference.name,
                module_nesting: parsed_reference.module_nesting,
                location: Range {
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                },
            }
        })
        .filter(|r| {
            dbg!(&collector.definitions);
            for constant_name in r.possible_fully_qualified_constants() {
                if def_set.contains(&constant_name) {
                    return false;
                }
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trivial_case() {
        let contents: String = String::from("Foo");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 4
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_nested_constant() {
        let contents: String = String::from("Foo::Bar");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 9
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_deeply_nested_constant() {
        let contents: String = String::from("Foo::Bar::Baz");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar::Baz"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 14
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_very_deeply_nested_constant() {
        let contents: String = String::from("Foo::Bar::Baz::Boo");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar::Baz::Boo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 19
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_class_definition() {
        let contents: String = String::from(
            "\
    class Foo
    end
            ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Foo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 7,
                    end_row: 1,
                    end_col: 10
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  Bar
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Bar"),
                module_nesting: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 3,
                    end_row: 2,
                    end_col: 6
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_deeply_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
    Baz
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Baz"),
                module_nesting: vec![
                    String::from("Foo::Bar"),
                    String::from("Foo")
                ],
                location: Range {
                    start_row: 3,
                    start_col: 5,
                    end_row: 3,
                    end_col: 8
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_very_deeply_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
    class Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Boo"),
                module_nesting: vec![
                    String::from("Foo::Bar::Baz"),
                    String::from("Foo::Bar"),
                    String::from("Foo")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 7,
                    end_row: 4,
                    end_col: 10
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  Bar
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Bar"),
                module_nesting: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 3,
                    end_row: 2,
                    end_col: 6
                }
            }],
            extract_from_contents(contents),
        );
    }

    #[test]
    fn test_deeply_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  module Bar
    Baz
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Baz"),
                module_nesting: vec![
                    String::from("Foo::Bar"),
                    String::from("Foo")
                ],
                location: Range {
                    start_row: 3,
                    start_col: 5,
                    end_row: 3,
                    end_col: 8
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_very_deeply_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  module Bar
    module Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Boo"),
                module_nesting: vec![
                    String::from("Foo::Bar::Baz"),
                    String::from("Foo::Bar"),
                    String::from("Foo")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 7,
                    end_row: 4,
                    end_col: 10
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_mixed_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  module Bar
    class Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Boo"),
                module_nesting: vec![
                    String::from("Foo::Bar::Baz"),
                    String::from("Foo::Bar"),
                    String::from("Foo")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 7,
                    end_row: 4,
                    end_col: 10
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_compact_style_class_definition_constant() {
        let contents: String = String::from(
            "\
class Foo::Bar
  Baz
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Baz"),
                module_nesting: vec![String::from("Foo::Bar")],
                location: Range {
                    start_row: 2,
                    start_col: 3,
                    end_row: 2,
                    end_col: 6
                }
            }],
            extract_from_contents(contents),
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_compact_style_with_nesting_class_definition_constant() {
        let contents: String = String::from(
            "\
class Foo::Bar
  module Baz
    Baz
  end
end
        ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("Baz"),
                module_nesting: vec![
                    String::from("Foo::Bar::Baz"),
                    String::from("Foo::Bar")
                ],
                location: Range {
                    start_row: 3,
                    start_col: 5,
                    end_row: 3,
                    end_col: 8
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_constant() {
        let contents: String = String::from("[Foo]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Foo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 2,
                    end_row: 1,
                    end_col: 5
                }
            },
            *reference
        );
    }
    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_multiple_constants() {
        let contents: String = String::from("[Foo, Bar]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let reference1 = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Foo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 2,
                    end_row: 1,
                    end_col: 5
                }
            },
            *reference1
        );
        let reference2 = references
            .get(1)
            .expect("There should be a reference at index 1");
        assert_eq!(
            Reference {
                name: String::from("Bar"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 7,
                    end_row: 1,
                    end_col: 10
                }
            },
            *reference2,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_nested_constant() {
        let contents: String = String::from("[Baz::Boo]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Baz::Boo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 2,
                    end_row: 1,
                    end_col: 10
                }
            },
            *reference,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_globally_referenced_constant() {
        let contents: String = String::from("::Foo");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("::Foo"),
                module_nesting: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 6
                }
            },
            *reference,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_metaprogrammatically_referenced_constant() {
        let contents: String = String::from("described_class::Foo");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 0);
    }

    #[test]
    fn test_ignore_local_constant() {
        let contents: String = String::from(
            "\
class Foo
  BAR = 1
  def use_bar
    puts BAR
  end
end
        ",
        );

        assert_eq!(extract_from_contents(contents), vec![]);
    }
}
