use inflector::Inflector;
use syn::Ident;
use syn::Item::Macro;

use crate::{code, GenerationConfig};

pub const FILE_SIGNATURE: &'static str = "/* This file is generated and managed by dsync */";

// TODO: handle postgres array types
// TODO: handle postgres tuple/record types

#[derive(Debug, Clone)]
pub struct ParsedColumnMacro {
    pub ty: String,
    pub name: Ident,
    pub is_nullable: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedTableMacro {
    pub name: Ident,
    pub struct_name: String,
    pub columns: Vec<ParsedColumnMacro>,
    pub primary_key_columns: Vec<Ident>,
    pub foreign_keys: Vec<(
        ForeignTableName,
        JoinColumn, /* this is the column from this table which maps to the foreign table's primary key*/
    )>,
    pub generated_code: String,
}

type ForeignTableName = Ident;
type JoinColumn = String;

#[derive(Debug, Clone)]
pub struct ParsedJoinMacro {
    pub table1: Ident,
    pub table2: Ident,
    pub table1_columns: String,
}

pub fn parse_and_generate_code(
    schema_file_contents: String,
    config: &GenerationConfig,
) -> anyhow::Result<Vec<ParsedTableMacro>> {
    let schema_file = syn::parse_file(&schema_file_contents).unwrap();

    let mut tables: Vec<ParsedTableMacro> = vec![];

    for item in schema_file.items {
        match item {
            Macro(macro_item) => {
                let macro_identifier = macro_item
                    .mac
                    .path
                    .segments
                    .last()
                    .expect("could not read identifier for macro")
                    .ident
                    .to_string();

                match macro_identifier.as_str() {
                    "table" => {
                        let parsed_table = handle_table_macro(macro_item, config);

                        // make sure the table isn't ignored
                        let table_options = config.table(parsed_table.name.to_string().as_str());
                        if !table_options.get_ignore() {
                            tables.push(parsed_table);
                        }
                    }
                    "joinable" => {
                        let parsed_join = handle_joinable_macro(macro_item);

                        for table in tables.iter_mut() {
                            if parsed_join
                                .table1
                                .to_string()
                                .eq(table.name.to_string().as_str())
                            {
                                table.foreign_keys.push((
                                    parsed_join.table2.clone(),
                                    parsed_join.table1_columns.clone(),
                                ));
                                break;
                            }
                        }
                    }
                    _ => {}
                };
            }
            _ => {}
        }
    }

    let mut tables_hm = std::collections::HashMap::new();

    for table in tables.iter_mut() {
        // table.generated_code = code::generate_table(table.clone(), config);
        tables_hm.insert(table.name.clone(), table);
    }

    for table in tables_hm.iter_mut() {
        // **table.1 = code::generate_table(table.clone(), config, &tables_hm);

        // (table.1).generated_code = String::new();
        table.1.generated_code = String::new();
    }

    Ok(tables)
}

fn handle_joinable_macro(macro_item: syn::ItemMacro) -> ParsedJoinMacro {
    // println!("joinable! macro: {:#?}", macro_item);

    let mut table1_name: Option<Ident> = None;
    let mut table2_name: Option<Ident> = None;
    let mut table2_join_column: Option<String> = None;

    for item in macro_item.mac.tokens.into_iter() {
        match &item {
            proc_macro2::TokenTree::Ident(ident) => {
                if table1_name.is_none() {
                    table1_name = Some(ident.clone());
                } else if table2_name.is_none() {
                    table2_name = Some(ident.clone());
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                if table1_name.is_none() || table2_name.is_none() {
                    panic!("Unsupported schema format! (encountered join column group too early)");
                } else {
                    table2_join_column = Some(group.stream().to_string());
                }
            }
            _ => {}
        }
    }

    ParsedJoinMacro {
        table1: table1_name
            .expect("Unsupported schema format! (could not determine first join table name)"),
        table2: table2_name
            .expect("Unsupported schema format! (could not determine second join table name)"),
        table1_columns: table2_join_column
            .expect("Unsupported schema format! (could not determine join column name)"),
    }
}

fn handle_table_macro(macro_item: syn::ItemMacro, config: &GenerationConfig) -> ParsedTableMacro {
    let mut table_name_ident: Option<Ident> = None;
    let mut table_primary_key_idents: Vec<Ident> = vec![];
    let mut table_columns: Vec<ParsedColumnMacro> = vec![];

    for item in macro_item.mac.tokens.into_iter() {
        match &item {
            proc_macro2::TokenTree::Ident(ident) => {
                table_name_ident = Some(ident.clone());
            }
            proc_macro2::TokenTree::Group(group) => {
                if group.delimiter() == proc_macro2::Delimiter::Parenthesis {
                    // primary keys group
                    // println!("GROUP-keys {:#?}", group);
                    for key_token in group.stream().into_iter() {
                        match key_token {
                            proc_macro2::TokenTree::Ident(ident) => {
                                table_primary_key_idents.push(ident)
                            }
                            _ => { /* ignore other tokens */ }
                        }
                    }
                } else if group.delimiter() == proc_macro2::Delimiter::Brace {
                    // columns group
                    // println!("GROUP-cols {:#?}", group);

                    let mut column_name: Option<Ident> = None;
                    let mut column_type: Option<Ident> = None;
                    let mut column_nullable: bool = false;

                    for column_tokens in group.stream().into_iter() {
                        match column_tokens {
                            proc_macro2::TokenTree::Ident(ident) => {
                                if column_name.is_none() {
                                    column_name = Some(ident.clone());
                                } else if ident.to_string().eq("Nullable") {
                                    column_nullable = true;
                                } else {
                                    column_type = Some(ident.clone());
                                }
                            }
                            proc_macro2::TokenTree::Punct(punct) => {
                                let char = punct.as_char();
                                if char == '-' || char == '>' {
                                    // nothing for arrow
                                    continue;
                                } else if char == ',' && column_name.is_some() && column_type.is_some() {
                                    // end of column def!

                                    // add the column
                                    table_columns.push(ParsedColumnMacro {
                                        name: column_name.expect("Unsupported schema format! (Invalid column name syntax)"),
                                        ty: schema_type_to_rust_type(column_type.expect("Unsupported schema format! (Invalid column type syntax)").to_string()),
                                        is_nullable: column_nullable,
                                    });

                                    // reset the properties
                                    column_name = None;
                                    column_type = None;
                                    column_nullable = false;
                                }
                            }
                            _ => panic!("Unsupported schema format! (Invalid column definition token in diesel table macro)")
                        }
                    }

                    if column_name.is_some() || column_type.is_some() || column_nullable {
                        // looks like a column was in the middle of being parsed, let's panic!
                        panic!(
                            "Unsupported schema format! (It seems a column was partially defined)"
                        );
                    }
                } else {
                    panic!("Unsupported schema format! (Invalid delimiter in diesel table macro group)")
                }
            }
            _ => {
                panic!("Unsupported schema format! (Invalid token tree item in diesel table macro)")
            }
        }
    }

    ParsedTableMacro {
        name: table_name_ident
            .clone()
            .expect("Unsupported schema format! (Could not extract table name from schema file)"),
        struct_name: table_name_ident
            .unwrap()
            .to_string()
            .to_pascal_case()
            .to_singular(),
        columns: table_columns,
        primary_key_columns: table_primary_key_idents,
        foreign_keys: vec![],
        generated_code: format!(
            "{FILE_SIGNATURE}\n\nFATAL ERROR: nothing was generated; this shouldn't be possible."
        ),
    }
}

// A function to translate diesel schema types into rust types
//
// reference: https://github.com/diesel-rs/diesel/blob/master/diesel/src/sql_types/mod.rs
// exact reference; https://github.com/diesel-rs/diesel/blob/292ac5c0ed6474f96734ba2e99b95b442064f69c/diesel/src/mysql/types/mod.rs
//
// The docs page for sql_types is comprehensive but it hides some alias types like Int4, Float8, etc.:
// https://docs.rs/diesel/latest/diesel/sql_types/index.html
fn schema_type_to_rust_type(schema_type: String) -> String {
    match schema_type.to_lowercase().as_str() {
        "unsigned" => panic!("Unsigned types are not yet supported, please open an issue if you need this feature!"), // TODO: deal with this later
        "inet" => panic!("Unsigned types are not yet supported, please open an issue if you need this feature!"), // TODO: deal with this later
        "cidr" => panic!("Unsigned types are not yet supported, please open an issue if you need this feature!"), // TODO: deal with this later

        // boolean
        "bool" => "bool",

        // numbers
        "tinyint" => "i8",
        "smallint" => "i16",
        "smallserial" => "i16",
        "int2" => "i16",
        "int4" => "i32",
        "int4range" => "(std::collections::Bound<i32>, std::collections::Bound<i32>)",
        "integer" => "i32",
        "serial" => "i32",
        "bigint" => "i64",
        "bigserial" => "i64",
        "int8" => "i64",
        "int8range" => "(std::collections::Bound<i64>, std::collections::Bound<i64>)",
        "float" => "f32",
        "float4" => "f32",
        "double" => "f64",
        "float8" => "f64",
        "numeric" => "bigdecimal::BigDecimal",
        "numrange" => "(std::collections::Bound<bigdecimal::BigDecimal>, std::collections::Bound<bigdecimal::BigDecimal>)",
        "decimal" => "bigdecimal::BigDecimal",

        // string
        "text" => "String",
        "varchar" => "String",
        "bpchar" => "String",
        "char" => "String",
        "tinytext" => "String",
        "mediumtext" => "String",
        "longtext" => "String",

        // bytes
        "binary" => "Vec<u8>",
        "bytea" => "Vec<u8>",
        "tinyblob" => "Vec<u8>",
        "blob" => "Vec<u8>",
        "mediumblob" => "Vec<u8>",
        "longblob" => "Vec<u8>",
        "varbinary" => "Vec<u8>",
        "bit" => "Vec<u8>",

        // date & time
        "date" => "chrono::NaiveDate",
        "daterange" => "(std::collections::Bound<chrono::NaiveDate>, std::collections::Bound<chrono::NaiveDate>)",
        "datetime" => "chrono::NaiveDateTime",
        "time" => "chrono::NaiveTime",
        "timestamp" => "chrono::NaiveDateTime",
        "tsrange" => "(std::collections::Bound<chrono::NaiveDateTime>, std::collections::Bound<chrono::NaiveDateTime>)",
        "timestamptz" => "chrono::DateTime<chrono::Utc>",
        "timestamptzsqlite" => "chrono::DateTime<chrono::Utc>",
        "tstzrange" => "(std::collections::Bound<chrono::DateTime<chrono::Utc>>, std::collections::Bound<chrono::DateTime<chrono::Utc>>)",

        // json
        "json" => "serde::Value",
        "jsonb" => "serde_json::Value",

        // misc
        "uuid" => "uuid::Uuid",
        "interval" => "PgInterval",
        "oid" => "u32",
        "money" => "PgMoney",
        "macaddr" => "[u8; 6]",
        // "inet" => "either ipnetwork::IpNetwork or ipnet::IpNet (TODO)",
        // "cidr" => "either ipnetwork::IpNetwork or ipnet::IpNet (TODO)",

        // panic if no type is found (this means generation is broken for this particular schema)
        _ => panic!("Unknown type found '{}', please report this!", schema_type)
    }.to_string()
}
