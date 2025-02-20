#![allow(clippy::too_many_arguments)]

use std::collections::BTreeMap;

use async_graphql::parser::types::{
  BaseType, ConstDirective, EnumType, FieldDefinition, InputObjectType, InputValueDefinition, SchemaDefinition,
  ServiceDocument, Type, TypeDefinition, TypeKind, TypeSystemDefinition, UnionType,
};
use async_graphql::parser::Positioned;
use async_graphql::Name;

use crate::config::group_by::GroupBy;
use crate::config::{self, Config, GraphQL, Http, RootSchema, Server, Union, Upstream};
use crate::directive::DirectiveCodec;
use crate::valid::{Valid as ValidDefault, ValidExtensions, ValidationError};

type Valid<A> = ValidDefault<A, String>;
fn from_document(doc: ServiceDocument) -> Valid<Config> {
  let schema_definition = schema_definition(&doc)?;

  Valid::Ok(Config {
    server: server(schema_definition)?,
    upstream: upstream(schema_definition)?,
    graphql: graphql(&doc)?,
  })
}
fn graphql(doc: &ServiceDocument) -> Valid<GraphQL> {
  let type_definitions: Vec<_> = doc
    .definitions
    .iter()
    .filter_map(|def| match def {
      TypeSystemDefinition::Type(type_definition) => Some(type_definition),
      _ => None,
    })
    .collect();

  let root_schema = to_root_schema(schema_definition(doc)?);

  Valid::Ok(GraphQL {
    schema: root_schema,
    types: to_types(&type_definitions)?,
    unions: to_union_types(&type_definitions),
  })
}

fn schema_definition(doc: &ServiceDocument) -> Valid<&SchemaDefinition> {
  let p = doc.definitions.iter().find_map(|def| match def {
    TypeSystemDefinition::Schema(schema_definition) => Some(&schema_definition.node),
    _ => None,
  });
  p.map_or_else(
    || Valid::fail("schema not found".to_string()).trace("schema"),
    Valid::Ok,
  )
}

fn process_schema_directives<'a, T: DirectiveCodec<'a, T> + Default>(
  schema_definition: &'a SchemaDefinition,
  directive_name: &str,
) -> Valid<T> {
  let mut res: Result<T, ValidationError<String>> = Valid::Ok(T::default());
  for directive in schema_definition.directives.iter() {
    if directive.node.name.node.as_ref() == directive_name {
      res = T::from_directive(&directive.node);
    }
  }
  res
}

fn server(schema_definition: &SchemaDefinition) -> Valid<Server> {
  process_schema_directives(schema_definition, "server")
}
fn upstream(schema_definition: &SchemaDefinition) -> Valid<Upstream> {
  process_schema_directives(schema_definition, "upstream")
}
fn to_root_schema(schema_definition: &SchemaDefinition) -> RootSchema {
  let query = schema_definition.query.as_ref().map(pos_name_to_string);
  let mutation = schema_definition.mutation.as_ref().map(pos_name_to_string);
  let subscription = schema_definition.subscription.as_ref().map(pos_name_to_string);

  RootSchema { query, mutation, subscription }
}
fn pos_name_to_string(pos: &Positioned<Name>) -> String {
  pos.node.to_string()
}
fn to_types(type_definitions: &Vec<&Positioned<TypeDefinition>>) -> Valid<BTreeMap<String, config::Type>> {
  let mut types = BTreeMap::new();
  for type_definition in type_definitions {
    let type_name = pos_name_to_string(&type_definition.node.name);
    let type_opt = match type_definition.node.kind.clone() {
      TypeKind::Object(object_type) => Some(to_object_type(
        &object_type.fields,
        &type_definition.node.description,
        false,
        &object_type.implements,
      )?),
      TypeKind::Interface(interface_type) => Some(to_object_type(
        &interface_type.fields,
        &type_definition.node.description,
        true,
        &interface_type.implements,
      )?),
      TypeKind::Enum(enum_type) => Some(to_enum(enum_type)),
      TypeKind::InputObject(input_object_type) => Some(to_input_object(input_object_type)?),
      TypeKind::Union(_) => None,
      TypeKind::Scalar => Some(to_scalar_type()),
    };
    if let Some(type_) = type_opt {
      types.insert(type_name, type_);
    }
  }
  Valid::Ok(types)
}
fn to_scalar_type() -> config::Type {
  config::Type { scalar: true, ..Default::default() }
}
fn to_union_types(type_definitions: &Vec<&Positioned<TypeDefinition>>) -> BTreeMap<String, Union> {
  let mut unions = BTreeMap::new();
  for type_definition in type_definitions {
    let type_name = pos_name_to_string(&type_definition.node.name);
    let type_opt = match type_definition.node.kind.clone() {
      TypeKind::Union(union_type) => to_union(
        union_type,
        &type_definition.node.description.as_ref().map(|pos| pos.node.clone()),
      ),
      _ => continue,
    };
    unions.insert(type_name, type_opt);
  }
  unions
}
fn to_object_type(
  fields: &Vec<Positioned<FieldDefinition>>,
  description: &Option<Positioned<String>>,
  interface: bool,
  implements: &[Positioned<Name>],
) -> Valid<config::Type> {
  let fields = to_fields(fields)?;
  let doc = description.as_ref().map(|pos| pos.node.clone());
  let implements = implements.iter().map(|pos| pos.node.to_string()).collect();
  Valid::Ok(config::Type { fields, doc, interface, implements, ..Default::default() })
}
fn to_enum(enum_type: EnumType) -> config::Type {
  let variants = enum_type
    .values
    .iter()
    .map(|value| value.node.value.to_string())
    .collect();
  config::Type { variants: Some(variants), ..Default::default() }
}
fn to_input_object(input_object_type: InputObjectType) -> Valid<config::Type> {
  let fields = to_input_object_fields(&input_object_type.fields)?;
  Valid::Ok(config::Type { fields, ..Default::default() })
}
fn to_fields_inner<T, F>(fields: &Vec<Positioned<T>>, transform: F) -> Valid<BTreeMap<String, config::Field>>
where
  F: Fn(&T) -> Valid<config::Field>,
  T: HasName,
{
  let mut parsed_fields = BTreeMap::new();
  for field in fields {
    let field_name = pos_name_to_string(field.node.name());
    let field_ = transform(&field.node)?;
    parsed_fields.insert(field_name, field_);
  }
  Valid::Ok(parsed_fields)
}
fn to_fields(fields: &Vec<Positioned<FieldDefinition>>) -> Valid<BTreeMap<String, config::Field>> {
  to_fields_inner(fields, to_field)
}
fn to_input_object_fields(
  input_object_fields: &Vec<Positioned<InputValueDefinition>>,
) -> Valid<BTreeMap<String, config::Field>> {
  to_fields_inner(input_object_fields, to_input_object_field)
}
fn to_field(field_definition: &FieldDefinition) -> Valid<config::Field> {
  to_common_field(
    &field_definition.ty.node,
    &field_definition.ty.node.base,
    field_definition.ty.node.nullable,
    to_args(field_definition),
    &field_definition.description,
    &field_definition.directives,
  )
}
fn to_input_object_field(field_definition: &InputValueDefinition) -> Valid<config::Field> {
  to_common_field(
    &field_definition.ty.node,
    &field_definition.ty.node.base,
    field_definition.ty.node.nullable,
    BTreeMap::new(),
    &field_definition.description,
    &field_definition.directives,
  )
}
fn to_common_field(
  type_: &Type,
  base: &BaseType,
  nullable: bool,
  args: BTreeMap<String, config::Arg>,
  description: &Option<Positioned<String>>,
  directives: &[Positioned<ConstDirective>],
) -> Valid<config::Field> {
  let type_of = to_type_of(type_);
  let list = matches!(&base, BaseType::List(_));
  let list_type_required = matches!(&base, BaseType::List(ty) if !ty.nullable);
  let doc = description.as_ref().map(|pos| pos.node.clone());
  let modify = to_modify(directives);
  let inline = to_inline(directives);
  let http = to_http(directives)?;
  let unsafe_operation = to_unsafe_operation(directives);
  let group_by = to_batch(directives);
  let const_field = to_const_field(directives);
  Valid::Ok(config::Field {
    type_of,
    list,
    required: !nullable,
    list_type_required,
    args,
    doc,
    modify,
    inline,
    http,
    unsafe_operation,
    group_by,
    const_field,
  })
}
fn to_unsafe_operation(directives: &[Positioned<ConstDirective>]) -> Option<config::Unsafe> {
  directives.iter().find_map(|directive| {
    if directive.node.name.node == "unsafe" {
      config::Unsafe::from_directive(&directive.node).ok()
    } else {
      None
    }
  })
}
fn to_type_of(type_: &Type) -> String {
  match &type_.base {
    BaseType::Named(name) => name.to_string(),
    BaseType::List(ty) => match &ty.base {
      BaseType::Named(name) => name.to_string(),
      _ => "".to_string(),
    },
  }
}
fn to_args(field_definition: &FieldDefinition) -> BTreeMap<String, config::Arg> {
  let mut args: BTreeMap<String, config::Arg> = BTreeMap::new();

  for arg in field_definition.arguments.iter() {
    let arg_name = pos_name_to_string(&arg.node.name);
    let arg_val = to_arg(&arg.node);
    args.insert(arg_name, arg_val);
  }

  args
}
fn to_arg(input_value_definition: &InputValueDefinition) -> config::Arg {
  let type_of = to_type_of(&input_value_definition.ty.node);
  let list = matches!(&input_value_definition.ty.node.base, BaseType::List(_));
  let required = !input_value_definition.ty.node.nullable;
  let doc = input_value_definition.description.as_ref().map(|pos| pos.node.clone());
  let modify = to_modify(&input_value_definition.directives);
  let default_value = if let Some(pos) = input_value_definition.default_value.as_ref() {
    let value = &pos.node;
    serde_json::to_value(value).ok()
  } else {
    None
  };
  config::Arg { type_of, list, required, doc, modify, default_value }
}
fn to_modify(directives: &[Positioned<ConstDirective>]) -> Option<config::ModifyField> {
  directives.iter().find_map(|directive| {
    if directive.node.name.node == "modify" {
      config::ModifyField::from_directive(&directive.node).ok()
    } else {
      None
    }
  })
}
fn to_inline(directives: &[Positioned<ConstDirective>]) -> Option<config::InlineType> {
  directives.iter().find_map(|directive| {
    if directive.node.name.node == "inline" {
      config::InlineType::from_directive(&directive.node).ok()
    } else {
      None
    }
  })
}
fn to_http(directives: &[Positioned<ConstDirective>]) -> Valid<Option<config::Http>> {
  for directive in directives {
    if directive.node.name.node == "http" {
      return Http::from_directive(&directive.node).map(Some);
    }
  }
  Valid::Ok(None)
}
fn to_union(union_type: UnionType, doc: &Option<String>) -> Union {
  let types = union_type
    .members
    .iter()
    .map(|member| member.node.to_string())
    .collect();
  Union { types, doc: doc.clone() }
}
fn to_batch(directives: &[Positioned<ConstDirective>]) -> Option<GroupBy> {
  directives.iter().find_map(|directive| {
    if directive.node.name.node == "groupBy" {
      GroupBy::from_directive(&directive.node).ok()
    } else {
      None
    }
  })
}
fn to_const_field(directives: &[Positioned<ConstDirective>]) -> Option<config::ConstField> {
  directives.iter().find_map(|directive| {
    if directive.node.name.node == "const" {
      config::ConstField::from_directive(&directive.node).ok()
    } else {
      None
    }
  })
}

trait HasName {
  fn name(&self) -> &Positioned<Name>;
}
impl HasName for FieldDefinition {
  fn name(&self) -> &Positioned<Name> {
    &self.name
  }
}
impl HasName for InputValueDefinition {
  fn name(&self) -> &Positioned<Name> {
    &self.name
  }
}

impl TryFrom<ServiceDocument> for Config {
  type Error = ValidationError<String>;

  fn try_from(value: ServiceDocument) -> Valid<Self> {
    from_document(value)
  }
}
