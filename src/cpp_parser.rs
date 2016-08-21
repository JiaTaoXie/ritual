extern crate clang;
use self::clang::*;

extern crate regex;
use self::regex::Regex;

use log;
use std;
use std::collections::HashMap;
use std::path::PathBuf;
use std::fs::File;

use utils::JoinWithString;

use cpp_data::{CppData, CppTypeData, CppTypeKind, CppClassField, EnumValue, CppOriginLocation,
               CppVisibility};
use cpp_method::{CppMethod, CppFunctionArgument, CppMethodKind, CppMethodClassMembership};
use cpp_type::{CppType, CppTypeBase, CppBuiltInNumericType, CppTypeIndirection,
               CppSpecificNumericTypeKind};
use cpp_operator::CppOperator;
use std::io::Write;

struct CppParser {
  config: CppParserConfig,
  types: Vec<CppTypeData>,
}

#[allow(dead_code)]
fn inspect_method(entity: Entity) {
  println!("{:?}", entity.get_display_name());
  println!("type: {:?}", entity.get_type());
  println!("return type: {:?}",
           entity.get_type().unwrap().get_result_type());
  println!("args:");
  for c in entity.get_arguments().unwrap() {
    println!("arg: name={} type={:?}",
             c.get_name().unwrap_or("[no name]".to_string()),
             c.get_type());
  }
}

#[allow(dead_code)]
fn dump_entity(entity: &Entity, level: i32) {
  for _ in 0..level {
    print!(". ");
  }
  println!("{:?}", entity);
  for child in entity.get_children() {
    dump_entity(&child, level + 1);
  }
}

fn get_origin_location(entity: Entity) -> Result<CppOriginLocation, String> {
  match entity.get_location() {
    Some(loc) => {
      let location = loc.get_presumed_location();
      Ok(CppOriginLocation {
        include_file_path: location.0,
        line: location.1,
        column: location.2,
      })
    }
    None => Err(format!("No info about location.")),
  }
}

fn get_template_arguments(entity: Entity) -> Vec<String> {
  entity.get_children()
    .into_iter()
    .filter(|c| c.get_kind() == EntityKind::TemplateTypeParameter)
    .enumerate()
    .map(|(i, c)| c.get_name().unwrap_or_else(|| format!("Type{}", i + 1)))
    .collect()
}


fn get_full_name(entity: Entity) -> Result<String, String> {
  let mut current_entity = entity;
  if let Some(mut s) = entity.get_name() {
    loop {
      if let Some(p) = current_entity.get_semantic_parent() {
        if p.get_kind() == EntityKind::ClassDecl || p.get_kind() == EntityKind::ClassTemplate ||
           p.get_kind() == EntityKind::StructDecl ||
           p.get_kind() == EntityKind::Namespace ||
           p.get_kind() == EntityKind::EnumDecl || p.get_kind() == EntityKind::Method ||
           p.get_kind() == EntityKind::ClassTemplatePartialSpecialization {
          match p.get_name() {
            Some(p_name) => s = format!("{}::{}", p_name, s),
            None => return Err(format!("Anonymous nested type")),
          }
          current_entity = p;
        } else {
          break;
        }
      } else {
        break;
      }
    }
    Ok(s)
  } else {
    Err(format!("Anonymous type"))
  }
}

#[derive(Clone, Debug)]
pub struct CppParserConfig {
  pub include_dirs: Vec<PathBuf>,
  pub header_name: String,
  pub tmp_cpp_path: PathBuf,
  pub name_blacklist: Vec<String>,
}

pub fn run(config: CppParserConfig) -> CppData {
  log::info(format!("clang version: {}", get_version()));
  log::info("Initializing clang...");
  let clang = Clang::new().unwrap_or_else(|err| panic!("clang init failed: {:?}", err));
  let index = Index::new(&clang, false, false);
  {
    let mut tmp_file = File::create(&config.tmp_cpp_path).unwrap();
    write!(tmp_file, "#include \"{}\"\n", config.header_name).unwrap();
  }
  let mut args =
    vec!["-fPIC".to_string(), "-Xclang".to_string(), "-detailed-preprocessing-record".to_string()];
  // let include_dirs_as_str = self.include_dirs.iter().map(|x| x.to_str().unwrap().to_string());
  for dir in &config.include_dirs {
    args.push("-I".to_string());
    args.push(dir.to_str().unwrap().to_string());
  }

  let tu = index.parser(&config.tmp_cpp_path)
    .arguments(&args)
    .parse()
    .unwrap_or_else(|err| panic!("clang parse failed: {:?}", err));
  let translation_unit = tu.get_entity();
  assert!(translation_unit.get_kind() == EntityKind::TranslationUnit);
  {
    let diagnostics = tu.get_diagnostics();
    if !diagnostics.is_empty() {
      log::warning("Diagnostics:");
      for diag in &diagnostics {
        log::warning(format!("{}", diag));
      }
    }
    if diagnostics.iter()
      .find(|d| {
        d.get_severity() == clang::diagnostic::Severity::Error ||
        d.get_severity() == clang::diagnostic::Severity::Fatal
      })
      .is_some() {
      panic!("terminated because of clang errors");
    }
  }
  log::info("Processing entities...");
  let mut parser = CppParser {
    types: Vec::new(),
    config: config.clone(),
  };
  parser.parse_types(translation_unit);
  let methods = parser.parse_methods(translation_unit);
  std::fs::remove_file(&config.tmp_cpp_path).unwrap();

  println!("test1: {:?}", methods);

  let good_methods = parser.check_integrity(methods);

  println!("test2: {:?}", good_methods);

  let template_instantiations = parser.find_template_instantiations(&good_methods);
  CppData {
    types: parser.types,
    methods: good_methods,
    template_instantiations: template_instantiations,
  }
}

impl CppParser {
  fn parse_unexposed_type(&self,
                          type1: Option<Type>,
                          string: Option<String>,
                          context_class: Option<Entity>,
                          context_method: Option<Entity>)
                          -> Result<CppType, String> {
    let template_class_regex = Regex::new(r"^([\w:]+)<(.+)>$").unwrap();
    let (is_const, name) = match type1 {
      Some(type1) => {
        let is_const = type1.is_const_qualified();
        let mut name = type1.get_display_name();
        let is_const_in_name = name.starts_with("const ");
        if is_const != is_const_in_name {
          panic!("const inconsistency: {}, {:?}", is_const, type1);
        }
        if is_const_in_name {
          name = name[6..].to_string();
        }
        if let Some(declaration) = type1.get_declaration() {
          if declaration.get_kind() == EntityKind::ClassDecl ||
             declaration.get_kind() == EntityKind::ClassTemplate ||
             declaration.get_kind() == EntityKind::StructDecl {
            if declaration.get_accessibility().unwrap_or(Accessibility::Public) !=
               Accessibility::Public {
              return Err(format!("Type uses private class ({})",
                                 get_full_name(declaration).unwrap()));
            }
            if let Some(matches) = template_class_regex.captures(name.as_ref()) {
              let mut arg_types = Vec::new();
              for arg in matches.at(2).unwrap().split(",") {
                match self.parse_unexposed_type(None,
                                                Some(arg.trim().to_string()),
                                                context_class,
                                                context_method) {
                  Ok(arg_type) => arg_types.push(arg_type),
                  Err(msg) => {
                    return Err(format!("Template argument of unexposed type is not parsed: {}: {}",
                                       arg,
                                       msg))
                  }
                }
              }
              return Ok(CppType {
                base: CppTypeBase::Class {
                  name: get_full_name(declaration).unwrap(),
                  template_arguments: Some(arg_types),
                },
                is_const: is_const,
                indirection: CppTypeIndirection::None,
              });
            } else {
              return Err(format!("Unexposed type has a declaration but is too complex: {}",
                                 name));
            }
          }
        }
        (is_const, name)
      }
      None => {
        let mut name = string.unwrap();
        let is_const_in_name = name.starts_with("const ");
        if is_const_in_name {
          name = name[6..].to_string();
        }
        (is_const_in_name, name)
      }
    };
    let re = Regex::new(r"^type-parameter-(\d+)-(\d+)$").unwrap();
    if let Some(matches) = re.captures(name.as_ref()) {
      return Ok(CppType {
        base: CppTypeBase::TemplateParameter {
          nested_level: matches.at(1).unwrap().parse().unwrap(),
          index: matches.at(2).unwrap().parse().unwrap(),
        },
        is_const: is_const,
        indirection: CppTypeIndirection::None,
      });
    }
    let mut method_has_template_arguments = false;
    if let Some(e) = context_method {
      let args = get_template_arguments(e);
      if !args.is_empty() {
        if let Some(index) = args.iter().position(|x| *x == name) {
          return Ok(CppType {
            base: CppTypeBase::TemplateParameter {
              nested_level: 0,
              index: index as i32,
            },
            is_const: is_const,
            indirection: CppTypeIndirection::None,
          });
        }
        method_has_template_arguments = true;
      }
    }
    if let Some(e) = context_class {
      if let Some(index) = get_template_arguments(e).iter().position(|x| *x == name) {
        return Ok(CppType {
          base: CppTypeBase::TemplateParameter {
            nested_level: if method_has_template_arguments { 1 } else { 0 },
            index: index as i32,
          },
          is_const: is_const,
          indirection: CppTypeIndirection::None,
        });
      }
    }
    let mut remaining_name: &str = name.as_ref();
    let mut type1 = CppType {
      is_const: is_const,
      indirection: CppTypeIndirection::None,
      base: CppTypeBase::Void,
    };
    if remaining_name.ends_with(" *") {
      type1.indirection = CppTypeIndirection::Ptr;
      remaining_name = remaining_name[0..remaining_name.len() - " *".len()].trim();
    }
    if remaining_name.ends_with(" &") {
      type1.indirection = CppTypeIndirection::Ref;
      remaining_name = remaining_name[0..remaining_name.len() - " &".len()].trim();
    }
    if remaining_name == "void" {
      return Ok(type1);
    }
    if let Some(x) = CppBuiltInNumericType::all()
      .iter()
      .find(|x| x.to_cpp_code() == remaining_name) {
      type1.base = CppTypeBase::BuiltInNumeric(x.clone());
      return Ok(type1);
    }
    if type1.indirection == CppTypeIndirection::Ptr ||
       type1.indirection == CppTypeIndirection::Ref {
      if let Ok(subtype) = self.parse_unexposed_type(None,
                                                     Some(remaining_name.to_string()),
                                                     context_class,
                                                     context_method) {
        return Ok(CppType {
          base: subtype.base,
          is_const: is_const,
          indirection: match type1.indirection {
            CppTypeIndirection::Ptr => {
              match subtype.indirection {
                CppTypeIndirection::None => CppTypeIndirection::Ptr,
                CppTypeIndirection::Ptr => CppTypeIndirection::PtrPtr,
                _ => return Err(format!("too much indirection")),
              }
            }
            CppTypeIndirection::Ref => {
              match subtype.indirection {
                CppTypeIndirection::None => CppTypeIndirection::Ref,
                CppTypeIndirection::Ptr => CppTypeIndirection::PtrRef,
                _ => return Err(format!("too much indirection")),
              }
            }
            _ => unreachable!(),
          },
        });
      }
    }
    if let Some(ref type_data) = self.types.iter().find(|x| &x.name == remaining_name) {
      match type_data.kind {
        CppTypeKind::Enum { .. } => {
          type1.base = CppTypeBase::Enum { name: remaining_name.to_string() }
        }
        CppTypeKind::Class { .. } => {
          type1.base = CppTypeBase::Class {
            name: remaining_name.to_string(),
            template_arguments: None,
          }
        }
      }
      return Ok(type1);
    }

    if let Some(matches) = template_class_regex.captures(remaining_name) {
      let class_name = matches.at(1).unwrap();
      if self.types.iter().find(|x| &x.name == class_name && x.is_class()).is_some() {
        let mut arg_types = Vec::new();
        for arg in matches.at(2).unwrap().split(",") {
          match self.parse_unexposed_type(None,
                                          Some(arg.trim().to_string()),
                                          context_class,
                                          context_method) {
            Ok(arg_type) => arg_types.push(arg_type),
            Err(msg) => {
              return Err(format!("Template argument of unexposed type is not parsed: {}: {}",
                                 arg,
                                 msg))
            }
          }
        }
        type1.base = CppTypeBase::Class {
          name: class_name.to_string(),
          template_arguments: Some(arg_types),
        };
        return Ok(type1);
      }
    } else {
      return Err(format!("Unexposed type has a declaration but is too complex: {}",
                         name));
    }

    return Err(format!("Unrecognized unexposed type: {}", name));
  }

  fn parse_type(&self,
                type1: Type,
                context_class: Option<Entity>,
                context_method: Option<Entity>)
                -> Result<CppType, String> {
    let parsed =
      try!(self.parse_canonical_type(type1.get_canonical_type(), context_class, context_method));
    if let CppTypeBase::BuiltInNumeric(..) = parsed.base {
      if parsed.indirection == CppTypeIndirection::None {
        let mut name = type1.get_display_name();
        if name.starts_with("const ") {
          name = name[6..].trim().to_string();
        }
        let real_type = match name.as_ref() {
          "qint8" | "int8_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 8,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: true },
            })
          }
          "quint8" | "uint8_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 8,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: false },
            })
          }
          "qint16" | "int16_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 16,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: true },
            })
          }
          "quint16" | "uint16_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 16,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: false },
            })
          }
          "qint32" | "int32_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 32,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: true },
            })
          }
          "quint32" | "uint32_t" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 32,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: false },
            })
          }
          "qint64" | "int64_t" | "qlonglong" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 64,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: true },
            })
          }
          "quint64" | "uint64_t" | "qulonglong" => {
            Some(CppTypeBase::SpecificNumeric {
              name: name.to_string(),
              bits: 64,
              kind: CppSpecificNumericTypeKind::Integer { is_signed: false },
            })
          }
          "qintptr" |
          "qptrdiff" |
          "QList_difference_type" => {
            Some(CppTypeBase::PointerSizedInteger {
              name: name.to_string(),
              is_signed: true,
            })
          }
          "quintptr" => {
            Some(CppTypeBase::PointerSizedInteger {
              name: name.to_string(),
              is_signed: false,
            })
          }
          _ => None,
        };
        if let Some(real_type) = real_type {
          return Ok(CppType {
            base: real_type,
            indirection: parsed.indirection,
            is_const: parsed.is_const,
          });
        }
      }
    }
    return Ok(parsed);
  }


  fn parse_canonical_type(&self,
                          type1: Type,
                          context_class: Option<Entity>,
                          context_method: Option<Entity>)
                          -> Result<CppType, String> {
    let is_const = type1.is_const_qualified();
    match type1.get_kind() {
      TypeKind::Void => {
        Ok(CppType {
          base: CppTypeBase::Void,
          is_const: is_const,
          indirection: CppTypeIndirection::None,
        })
      }
      TypeKind::Bool |
      TypeKind::CharS |
      TypeKind::CharU |
      TypeKind::SChar |
      TypeKind::UChar |
      TypeKind::WChar |
      TypeKind::Char16 |
      TypeKind::Char32 |
      TypeKind::Short |
      TypeKind::UShort |
      TypeKind::Int |
      TypeKind::UInt |
      TypeKind::Long |
      TypeKind::ULong |
      TypeKind::LongLong |
      TypeKind::ULongLong |
      TypeKind::Int128 |
      TypeKind::UInt128 |
      TypeKind::Float |
      TypeKind::Double |
      TypeKind::LongDouble => {
        Ok(CppType {
          base: CppTypeBase::BuiltInNumeric(match type1.get_kind() {
            TypeKind::Bool => CppBuiltInNumericType::Bool,
            TypeKind::CharS => CppBuiltInNumericType::Char,
            TypeKind::CharU => CppBuiltInNumericType::Char,
            TypeKind::SChar => CppBuiltInNumericType::SChar,
            TypeKind::UChar => CppBuiltInNumericType::UChar,
            TypeKind::WChar => CppBuiltInNumericType::WChar,
            TypeKind::Char16 => CppBuiltInNumericType::Char16,
            TypeKind::Char32 => CppBuiltInNumericType::Char32,
            TypeKind::Short => CppBuiltInNumericType::Short,
            TypeKind::UShort => CppBuiltInNumericType::UShort,
            TypeKind::Int => CppBuiltInNumericType::Int,
            TypeKind::UInt => CppBuiltInNumericType::UInt,
            TypeKind::Long => CppBuiltInNumericType::Long,
            TypeKind::ULong => CppBuiltInNumericType::ULong,
            TypeKind::LongLong => CppBuiltInNumericType::LongLong,
            TypeKind::ULongLong => CppBuiltInNumericType::ULongLong,
            TypeKind::Int128 => CppBuiltInNumericType::Int128,
            TypeKind::UInt128 => CppBuiltInNumericType::UInt128,
            TypeKind::Float => CppBuiltInNumericType::Float,
            TypeKind::Double => CppBuiltInNumericType::Double,
            TypeKind::LongDouble => CppBuiltInNumericType::LongDouble,
            _ => unreachable!(),
          }),
          is_const: is_const,
          indirection: CppTypeIndirection::None,
        })
      }
      TypeKind::Enum => {
        Ok(CppType {
          base: CppTypeBase::Enum {
            name: get_full_name(type1.get_declaration().unwrap()).unwrap(),
          },
          is_const: is_const,
          indirection: CppTypeIndirection::None,
        })
      }
      TypeKind::Record => {
        let declaration = type1.get_declaration().unwrap();
        if declaration.get_accessibility().unwrap_or(Accessibility::Public) !=
           Accessibility::Public {
          return Err(format!("Type uses private class ({})",
                             get_full_name(declaration).unwrap_or("unnamed".to_string())));
        }
        match get_full_name(declaration) {
          Ok(declaration_name) => {
            let template_arguments = match type1.get_template_argument_types() {
              None => None,
              Some(arg_types) => {
                let mut r = Vec::new();
                if arg_types.is_empty() {
                  panic!("arg_types is empty");
                }
                for arg_type in arg_types {
                  match arg_type {
                    None => return Err(format!("Template argument is None")),
                    Some(arg_type) => {
                      match self.parse_type(arg_type, context_class, context_method) {
                        Ok(parsed_type) => r.push(parsed_type),
                        Err(msg) => {
                          return Err(format!("Invalid template argument: {:?}: {}", arg_type, msg))
                        }
                      }
                    }
                  }
                }
                Some(r)
              }
            };

            Ok(CppType {
              base: CppTypeBase::Class {
                name: declaration_name,
                template_arguments: template_arguments,
              },
              is_const: is_const,
              indirection: CppTypeIndirection::None,
            })

          }
          Err(msg) => Err(format!("get_full_name failed: {}", msg)),
        }
      }
      TypeKind::FunctionPrototype => {
        let mut arguments = Vec::new();
        for arg_type in type1.get_argument_types().unwrap() {
          match self.parse_type(arg_type, context_class, context_method) {
            Ok(t) => arguments.push(t),
            Err(msg) => {
              return Err(format!("Failed to parse function type's argument type: {:?}: {}",
                                 arg_type,
                                 msg))
            }
          }
        }
        let return_type = match self.parse_type(type1.get_result_type().unwrap(),
                                                context_class,
                                                context_method) {
          Ok(t) => Box::new(t),
          Err(msg) => {
            return Err(format!("Failed to parse function type's argument type: {:?}: {}",
                               type1.get_result_type().unwrap(),
                               msg))
          }
        };
        Ok(CppType {
          base: CppTypeBase::FunctionPointer {
            return_type: return_type,
            arguments: arguments,
            allows_variadic_arguments: type1.is_variadic(),
          },
          is_const: is_const,
          indirection: CppTypeIndirection::None,
        })
      }
      TypeKind::Pointer |
      TypeKind::LValueReference |
      TypeKind::RValueReference => {
        match type1.get_pointee_type() {
          Some(pointee) => {
            match self.parse_type(pointee, context_class, context_method) {
              Ok(result) => {
                let new_indirection = match type1.get_kind() {
                  TypeKind::Pointer => {
                    match result.indirection {
                      CppTypeIndirection::None => {
                        match result.base {
                          CppTypeBase::FunctionPointer { .. } => Ok(CppTypeIndirection::None),
                          _ => Ok(CppTypeIndirection::Ptr),
                        }
                      }
                      CppTypeIndirection::Ptr => Ok(CppTypeIndirection::PtrPtr),
                      _ => {
                        Err(format!("Unsupported level of indirection: pointer to {:?}",
                                    result.indirection))
                      }
                    }
                  }
                  TypeKind::LValueReference => {
                    match result.indirection {
                      CppTypeIndirection::None => Ok(CppTypeIndirection::Ref),
                      CppTypeIndirection::Ptr => Ok(CppTypeIndirection::PtrRef),
                      _ => {
                        Err(format!("Unsupported level of indirection: reference to {:?}",
                                    result.indirection))
                      }
                    }
                  }
                  TypeKind::RValueReference => {
                    if result.indirection == CppTypeIndirection::None {
                      Ok(CppTypeIndirection::Ref)
                    } else {
                      Err(format!("Unsupported level of indirection: r-value reference to {:?}",
                                  result.indirection))
                    }
                  }
                  _ => unreachable!(),
                };
                match new_indirection {
                  Ok(new_indirection) => Ok(CppType { indirection: new_indirection, ..result }),
                  Err(msg) => Err(msg),
                }
              }
              Err(msg) => Err(msg),
            }
          }
          None => Err("can't get pointee type".to_string()),
        }
      }
      TypeKind::Unexposed => {
        self.parse_unexposed_type(Some(type1), None, context_class, context_method)
      }
      _ => Err(format!("Unsupported kind of type: {:?}", type1.get_kind())),
    }
  }

  fn parse_function(&self, entity: Entity) -> Result<CppMethod, String> {
    let (class_name, class_entity) = match entity.get_semantic_parent() {
      Some(p) => {
        match p.get_kind() {
          EntityKind::ClassDecl |
          EntityKind::ClassTemplate |
          EntityKind::StructDecl |
          EntityKind::ClassTemplatePartialSpecialization => {
            match get_full_name(p) {
              Ok(class_name) => (Some(class_name), Some(p)),
              Err(msg) => {
                panic!("function parent is a class but it doesn't have a name: {}",
                       msg)
              }
            }
          }
          _ => (None, None),
        }
      }
      None => (None, None),
    };
    let return_type = entity.get_type()
      .unwrap_or_else(|| panic!("failed to get function type"))
      .get_result_type()
      .unwrap_or_else(|| panic!("failed to get function return type"));
    let return_type_parsed = match self.parse_type(return_type, class_entity, Some(entity)) {
      Ok(x) => x,
      Err(msg) => {
        return Err(format!("Can't parse return type: {:?}: {}", return_type, msg));
      }
    };
    let mut arguments = Vec::new();
    let argument_entities = match entity.get_kind() {
      EntityKind::FunctionTemplate => {
        entity.get_children().into_iter().filter(|c| c.get_kind() == EntityKind::ParmDecl).collect()
      }
      _ => entity.get_arguments().unwrap(),
    };
    let template_arguments = match entity.get_kind() {
      EntityKind::FunctionTemplate => {
        if entity.get_children()
          .into_iter()
          .find(|c| c.get_kind() == EntityKind::NonTypeTemplateParameter)
          .is_some() {
          return Err(format!("Non-type template parameter is not supported"));
        }
        Some(get_template_arguments(entity))
      }
      _ => None,
    };

    for (argument_number, argument_entity) in argument_entities.into_iter()
      .enumerate() {
      let name = argument_entity.get_name().unwrap_or(format!("arg{}", argument_number + 1));
      let type1 = self.parse_type(argument_entity.get_type().unwrap(),
                                  class_entity,
                                  Some(entity));

      match type1 {
        Ok(argument_type) => {
          arguments.push(CppFunctionArgument {
            name: name,
            argument_type: argument_type,
            has_default_value: argument_entity.get_range()
              .unwrap()
              .tokenize()
              .iter()
              .find(|t| t.get_spelling() == "=")
              .is_some(),
          });
        }
        Err(msg) => {
          return Err(format!("Can't parse argument type: {}: {:?}: {}",
                             name,
                             argument_entity.get_type().unwrap(),
                             msg));
        }
      }
    }
    let mut name = entity.get_name().unwrap_or_else(|| panic!("failed to get function name"));
    if name.contains('<') {
      let regex = Regex::new(r"^([\w~]+)<[^<>]+>$").unwrap();
      if let Some(matches) = regex.captures(name.clone().as_ref()) {
        log::warning(format!("Fixing malformed method name: {}", name));
        name = matches.at(1).unwrap().to_string();
      }
    }
    if let Some(parent) = entity.get_semantic_parent() {
      if parent.get_kind() == EntityKind::Namespace {
        name = format!("{}::{}", get_full_name(parent).unwrap(), name);
      }
    }
    let allows_variadic_arguments = entity.is_variadic();
    let has_this_argument = class_name.is_some() && !entity.is_static_method();
    let real_arguments_count = arguments.len() as i32 + if has_this_argument { 1 } else { 0 };
    let mut method_operator = None;
    if name.starts_with("operator") {
      let name_suffix = name["operator".len()..].trim();
      let mut name_matches = false;
      for operator in CppOperator::all() {
        let info = operator.info();
        if let Some(s) = info.function_name_suffix {
          if s == name_suffix {
            name_matches = true;
            if info.allows_variadic_arguments || info.arguments_count == real_arguments_count {
              method_operator = Some(operator.clone());
              break;
            }
          }
        }
      }
      if method_operator.is_none() && name_matches {
        return Err(format!("This method is recognized as operator but arguments do not match \
                            its signature."));
      }
    }
    if method_operator.is_none() && name.starts_with("operator ") {
      let op = name["operator ".len()..].trim();
      match self.parse_unexposed_type(None, Some(op.to_string()), class_entity, Some(entity)) {
        Ok(t) => method_operator = Some(CppOperator::Conversion(t)),
        Err(msg) => {
          panic!("Unknown operator: '{}' (method name: {}); error: {}",
                 op,
                 name,
                 msg)
        }
      }
    }

    Ok(CppMethod {
      name: name,
      operator: method_operator,
      class_membership: match class_name {
        Some(class_name) => {
          Some(CppMethodClassMembership {
            kind: match entity.get_kind() {
              EntityKind::Constructor => CppMethodKind::Constructor,
              EntityKind::Destructor => CppMethodKind::Destructor,
              _ => CppMethodKind::Regular,
            },
            is_virtual: entity.is_virtual_method(),
            is_pure_virtual: entity.is_pure_virtual_method(),
            is_const: entity.is_const_method(),
            is_static: entity.is_static_method(),
            visibility: match entity.get_accessibility().unwrap_or(Accessibility::Public) {
              Accessibility::Public => CppVisibility::Public,
              Accessibility::Protected => CppVisibility::Protected,
              Accessibility::Private => CppVisibility::Private,
            },
            is_signal: false, // TODO: get list of signals and slots at runtime
            class_type: match self.types.iter().find(|x| &x.name == &class_name) {
              Some(info) => info.default_class_type(),
              None => return Err(format!("Unknown class type: {}", class_name)),
            },
          })
        }
        None => None,
      },
      arguments: arguments,
      allows_variadic_arguments: allows_variadic_arguments,
      return_type: return_type_parsed,
      include_file: self.entity_include_file(entity).unwrap(),
      origin_location: Some(get_origin_location(entity).unwrap()),
      template_arguments: template_arguments,
    })
  }

  fn parse_enum(&self, entity: Entity) -> Result<CppTypeData, String> {
    let mut values = Vec::new();
    for child in entity.get_children() {
      if child.get_kind() == EntityKind::EnumConstantDecl {
        values.push(EnumValue {
          name: child.get_name().unwrap(),
          value: child.get_enum_constant_value().unwrap().0,
        });
      }
    }
    Ok(CppTypeData {
      name: get_full_name(entity).unwrap(),
      include_file: match self.entity_include_file(entity) {
        Some(x) => x.clone(),
        None => {
          return Err(format!("Origin of type is unknown: {}\nentity: {:?}\n",
                             get_full_name(entity).unwrap(),
                             entity))
        }
      },
      origin_location: get_origin_location(entity).unwrap(),
      kind: CppTypeKind::Enum { values: values },
    })
  }

  fn parse_class(&self, entity: Entity) -> Result<CppTypeData, String> {
    let mut fields = Vec::new();
    let mut bases = Vec::new();
    let template_arguments = get_template_arguments(entity);
    for child in entity.get_children() {
      if child.get_kind() == EntityKind::FieldDecl {
        match self.parse_type(child.get_type().unwrap(), Some(entity), None) {
          Ok(field_type) => {
            fields.push(CppClassField {
              name: child.get_name().unwrap(),
              field_type: field_type,
              visibility: match entity.get_accessibility().unwrap_or(Accessibility::Public) {
                Accessibility::Public => CppVisibility::Public,
                Accessibility::Protected => CppVisibility::Protected,
                Accessibility::Private => CppVisibility::Private,
              },
            });
          }
          Err(msg) => {
            log::warning(format!("Can't parse field type: {}::{}: {}",
                                 get_full_name(entity).unwrap(),
                                 child.get_name().unwrap(),
                                 msg))
          }
        };
      }
      if child.get_kind() == EntityKind::BaseSpecifier {
        let base_type = match self.parse_type(child.get_type().unwrap(), None, None) {
          Ok(r) => r,
          Err(msg) => return Err(format!("Can't parse base class type: {}", msg)),
        };
        bases.push(base_type);
      }
      if child.get_kind() == EntityKind::NonTypeTemplateParameter {
        return Err(format!("Non-type template parameter is not supported"));
      }
    }
    let size = match entity.get_type() {
      Some(type1) => type1.get_sizeof().ok().map(|x| x as i32),
      None => None,
    };
    Ok(CppTypeData {
      name: get_full_name(entity).unwrap(),
      include_file: match self.entity_include_file(entity) {
        Some(x) => x.clone(),
        None => {
          return Err(format!("Origin of type is unknown: {}\nentity: {:?}\n",
                             get_full_name(entity).unwrap(),
                             entity))
        }
      },
      origin_location: get_origin_location(entity).unwrap(),
      kind: CppTypeKind::Class {
        size: size,
        bases: bases,
        fields: fields,
        template_arguments: if entity.get_kind() == EntityKind::ClassTemplate {
          if template_arguments.is_empty() {
            panic!("missing template arguments");
          }
          Some(template_arguments)
        } else {
          if !template_arguments.is_empty() {
            panic!("unexpected template arguments");
          }
          None
        },
      },
    })
  }

  fn entity_include_path(&self, entity: Entity) -> Option<String> {
    if let Some(location) = entity.get_location() {
      let file_path = location.get_presumed_location().0;
      if file_path.is_empty() {
        log::noisy(format!("empty file path: {:?}", entity.get_kind()));
        None
      } else {
        Some(file_path)
      }
    } else {
      None
    }
  }

  fn entity_include_file(&self, entity: Entity) -> Option<String> {
    match self.entity_include_path(entity) {
      Some(file_path) => {
        let file_path_buf = PathBuf::from(file_path.clone());
        Some(file_path_buf.file_name()
          .unwrap()
          .to_str()
          .unwrap()
          .to_string())
      }
      None => None,
    }
  }

  fn should_process_entity(&self, entity: Entity) -> bool {
    if let Some(file_path) = self.entity_include_path(entity) {
      let file_path_buf = PathBuf::from(&file_path);
      if self.config.include_dirs.iter().find(|dir| file_path_buf.starts_with(dir)).is_none() {
        log::noisy(format!("skipping entities from {}", file_path));
        return false;
      }
    }
    if let Ok(full_name) = get_full_name(entity) {
      if self.config.name_blacklist.iter().find(|&x| x == &full_name).is_some() {
        log::info(format!("Skipping blacklisted entity: {}", full_name));
        return false;
      }
    }
    true
  }


  fn parse_types(&mut self, entity: Entity) {
    if !self.should_process_entity(entity) {
      return;
    }
    match entity.get_kind() {
      EntityKind::EnumDecl => {
        if entity.get_accessibility() == Some(Accessibility::Private) {
          return; // skipping private stuff
        }
        if entity.get_name().is_some() && entity.is_definition() {
          match self.parse_enum(entity) {
            Ok(r) => {
              if self.types.iter().find(|x| x.name == r.name).is_some() {
                panic!("repeating class declaration: {:?}", entity);
              }
              self.types.push(r);
            }
            Err(msg) => {
              log::warning(format!("Failed to parse enum: {}\nentity: {:?}\nerror: {}\n",
                                   get_full_name(entity).unwrap(),
                                   entity,
                                   msg));
            }
          }
        }
      }
      EntityKind::ClassDecl |
      EntityKind::ClassTemplate |
      EntityKind::StructDecl => {
        if entity.get_accessibility() == Some(Accessibility::Private) {
          return; // skipping private stuff
        }
        let ok = entity.get_name().is_some() && // not an anonymous struct
        entity.is_definition() && // not a forward declaration
        entity.get_template().is_none(); // not a template specialization
        if ok {
          match self.parse_class(entity) {
            Ok(r) => {
              if self.types.iter().find(|x| x.name == r.name).is_some() {
                panic!("repeating class declaration: {:?}", entity);
              }
              self.types.push(r);
            }
            Err(msg) => {
              log::warning(format!("Failed to parse class: {}\nentity: {:?}\nerror: {}\n",
                                   get_full_name(entity).unwrap(),
                                   entity,
                                   msg));
            }
          }
        }
      }
      _ => {}
    }
    for c in entity.get_children() {
      self.parse_types(c);
    }
  }

  fn parse_methods(&self, entity: Entity) -> Vec<CppMethod> {
    let mut methods = Vec::new();
    if !self.should_process_entity(entity) {
      return methods;
    }
    match entity.get_kind() {
      EntityKind::FunctionDecl |
      EntityKind::Method |
      EntityKind::Constructor |
      EntityKind::Destructor |
      EntityKind::ConversionFunction |
      EntityKind::FunctionTemplate => {
        if entity.get_canonical_entity() == entity {
          match self.parse_function(entity) {
            Ok(r) => {
              methods.push(r);
            }
            Err(msg) => {
              let full_name = get_full_name(entity).unwrap();
              let message = format!("Failed to parse method: {}\nentity: {:?}\nerror: {}\n",
                                    full_name,
                                    entity,
                                    msg);
              log::warning(message.as_ref());
            }
          }
        }
      }
      _ => {}
    }
    for c in entity.get_children() {
      methods.append(&mut self.parse_methods(c));
    }
    methods
  }

  fn check_type_integrity(&self, type1: &CppType) -> Result<(), String> {
    match type1.base {
      CppTypeBase::Void |
      CppTypeBase::BuiltInNumeric(..) |
      CppTypeBase::SpecificNumeric { .. } |
      CppTypeBase::PointerSizedInteger { .. } => {}
      CppTypeBase::Enum { ref name } => {
        if self.types.iter().find(|x| &x.name == name).is_none() {
          return Err(format!("unknown type: {}", name));
        }
      }
      CppTypeBase::Class { ref name, ref template_arguments } => {
        if self.types.iter().find(|x| &x.name == name).is_none() {
          return Err(format!("unknown type: {}", name));
        }
        if let &Some(ref args) = template_arguments {
          for arg in args {
            if let Err(msg) = self.check_type_integrity(&arg) {
              return Err(msg);
            }
          }
        }
      }
      CppTypeBase::FunctionPointer { ref return_type, ref arguments, .. } => {
        if let Err(msg) = self.check_type_integrity(return_type) {
          return Err(msg);
        }
        for arg in arguments {
          if let Err(msg) = self.check_type_integrity(arg) {
            return Err(msg);
          }
        }
      }
      CppTypeBase::TemplateParameter { .. } => {}
    }
    Ok(())
  }

  fn check_integrity(&self, methods: Vec<CppMethod>) -> Vec<CppMethod> {
    log::info("Checking data integrity");
    let good_methods = methods.into_iter()
      .filter(|method| {
        if let Err(msg) = self.check_type_integrity(&method.return_type
          .clone()) {
          log::warning(format!("Method is removed: {}: {}", method.short_text(), msg));
          return false;
        }
        for arg in &method.arguments {
          if let Err(msg) = self.check_type_integrity(&arg.argument_type) {
            log::warning(format!("Method is removed: {}: {}", method.short_text(), msg));
            return false;
          }
        }
        true
      })
      .collect();
    for t in &self.types {
      if let CppTypeKind::Class { ref bases, .. } = t.kind {
        for base in bases {
          if let Err(msg) = self.check_type_integrity(&base) {
            log::warning(format!("Class {}: base class type {:?}: {}", t.name, base, msg));
          }
        }
      }
    }
    good_methods
  }

  fn find_template_instantiations(&self,
                                  methods: &Vec<CppMethod>)
                                  -> HashMap<String, Vec<Vec<CppType>>> {

    fn check_type(type1: &CppType, result: &mut HashMap<String, Vec<Vec<CppType>>>) {
      if let CppTypeBase::Class { ref name, ref template_arguments } = type1.base {
        if let &Some(ref template_arguments) = template_arguments {
          if template_arguments.iter().find(|x| !x.base.is_template_parameter()).is_some() {
            if !result.contains_key(name) {
              result.insert(name.clone(), Vec::new());
            }
            if result.get(name).unwrap().iter().find(|x| x == &template_arguments).is_none() {
              result.get_mut(name).unwrap().push(template_arguments.clone());
            }
            for arg in template_arguments {
              check_type(arg, result);
            }
          }
        }
      }
    }
    let mut result = HashMap::new();
    for m in methods {
      check_type(&m.return_type, &mut result);
      for arg in &m.arguments {
        check_type(&arg.argument_type, &mut result);
      }
    }
    for t in &self.types {
      if let CppTypeKind::Class { ref bases, .. } = t.kind {
        for base in bases {
          check_type(&base, &mut result);
        }
      }
    }
    if result.is_empty() {
      log::info("No template instantiations detected.");
    } else {
      log::info("Detected template instantiations:");
    }
    for (class_name, instantiations) in &result {
      println!("Class: {}", class_name);
      if let Some(ref type_info) = self.types.iter().find(|x| &x.name == class_name) {
        if let CppTypeKind::Class { ref template_arguments, .. } = type_info.kind {
          if let &Some(ref template_arguments) = template_arguments {
            let valid_length = template_arguments.len();
            for ins in instantiations {
              println!("    {}<{}>",
                       class_name,
                       ins.iter()
                         .map(|t| t.to_cpp_code(None).unwrap_or_else(|_| format!("{:?}", t)))
                         .join(", "));
              if ins.len() != valid_length {
                panic!("template arguments count mismatch: {}: {:?} vs {:?}",
                       class_name,
                       template_arguments,
                       ins);
              }
            }
          } else {
            panic!("template class is not a template class: {}", class_name);
          }
        } else {
          panic!("template class is not a class: {}", class_name);
        }
      } else {
        panic!("template class is not available: {}", class_name);
      }
    }
    result
  }
}
