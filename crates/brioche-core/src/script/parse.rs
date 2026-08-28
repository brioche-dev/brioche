use std::collections::HashMap;

use biome_rowan::{AstNode as _, AstNodeList as _, AstSeparatedList as _};

use crate::{path::RelativePath, project::StaticQuery};

pub struct ScriptAst {
    module: biome_js_syntax::JsModule,
}

#[must_use]
pub fn parse_script(source: &str) -> ScriptAst {
    let module = biome_js_parser::parse(
        source,
        biome_languages::JsFileSource::ts_restricted(),
        biome_js_parser::JsParserOptions::default(),
    )
    .cast::<biome_js_syntax::JsModule>()
    .unwrap();
    let module = module.tree();
    ScriptAst { module }
}

#[derive(Debug, Clone)]
pub struct ScriptExportValue {
    pub value: serde_json::Value,
    pub range: TextRange,
}

pub fn get_export_value(
    script: &ScriptAst,
    export_name: &str,
) -> Result<Option<ScriptExportValue>, ScriptParseError> {
    let export_with_name = script.module.items().iter().find_map(move |item| {
        let export = item.as_js_export()?;
        let export_clause = export.export_clause().ok()?;
        let declaration = export_clause.as_any_js_declaration_clause()?;
        let var_declaration = declaration.as_js_variable_declaration_clause()?;
        let var_declaration = var_declaration.declaration().ok()?;

        var_declaration.declarators().iter().find_map(|declarator| {
            let declarator = declarator.ok()?;
            let id = declarator.id().ok()?;
            let id = id.as_any_js_binding()?.as_js_identifier_binding()?;
            let id_name = id.name_token().ok()?;

            if id_name.text_trimmed() == export_name {
                Some((declarator, id_name))
            } else {
                None
            }
        })
    });
    let Some((export_declarator, id_name)) = export_with_name else {
        return Ok(None);
    };

    let export_initializer =
        export_declarator
            .initializer()
            .ok_or_else(|| ScriptParseError::UnsupportedExport {
                range: export_declarator.range().into(),
                reason: "expected initializer".into(),
            })?;
    let export_expr =
        export_initializer
            .expression()
            .map_err(|error| ScriptParseError::SyntaxError {
                error,
                range: export_initializer.range().into(),
            })?;

    let value = expression_to_json(&export_expr, None)?;
    Ok(Some(ScriptExportValue {
        value,
        range: id_name.text_range().into(),
    }))
}

#[derive(Debug, Clone)]
pub struct ScriptImport {
    pub specifier: String,
    pub range: TextRange,
}

pub fn find_imports(
    script: &ScriptAst,
) -> impl Iterator<Item = Result<ScriptImport, ScriptParseError>> {
    find_top_level_imports(script).chain(find_dynamic_imports(script))
}

fn find_top_level_imports(
    script: &ScriptAst,
) -> impl Iterator<Item = Result<ScriptImport, ScriptParseError>> {
    script
        .module
        .items()
        .iter()
        .map(move |item| {
            let import_source = match &item {
                biome_js_syntax::AnyJsModuleItem::JsExport(export_item) => {
                    let export_clause = export_item.export_clause().map_err(|error| {
                        ScriptParseError::SyntaxError {
                            error,
                            range: export_item.syntax().text_range_with_trivia().into(),
                        }
                    })?;
                    match export_clause {
                        biome_js_syntax::AnyJsExportClause::JsExportFromClause(clause) => {
                            clause.source()
                        }
                        biome_js_syntax::AnyJsExportClause::JsExportNamedFromClause(clause) => {
                            clause.source()
                        }
                        biome_js_syntax::AnyJsExportClause::AnyJsDeclarationClause(_)
                        | biome_js_syntax::AnyJsExportClause::JsExportDefaultDeclarationClause(_)
                        | biome_js_syntax::AnyJsExportClause::JsExportDefaultExpressionClause(_)
                        | biome_js_syntax::AnyJsExportClause::JsExportNamedClause(_)
                        | biome_js_syntax::AnyJsExportClause::TsExportAsNamespaceClause(_)
                        | biome_js_syntax::AnyJsExportClause::TsExportAssignmentClause(_)
                        | biome_js_syntax::AnyJsExportClause::TsExportDeclareClause(_) => {
                            // Not an export from another module
                            return Ok(None);
                        }
                    }
                }
                biome_js_syntax::AnyJsModuleItem::JsImport(import_item) => {
                    let import_clause = import_item.import_clause().map_err(|error| {
                        ScriptParseError::SyntaxError {
                            error,
                            range: import_item.syntax().text_range_with_trivia().into(),
                        }
                    })?;
                    import_clause.source().map(Into::into)
                }
                biome_js_syntax::AnyJsModuleItem::AnyJsStatement(_) => {
                    // Not an import or export statement
                    return Ok(None);
                }
            };

            let import_source = import_source.map_err(|error| ScriptParseError::SyntaxError {
                error,
                range: item.syntax().text_range_with_trivia().into(),
            })?;
            let import_source = match import_source {
                biome_js_syntax::AnyJsModuleSource::JsModuleSource(import_source) => import_source,
                biome_js_syntax::AnyJsModuleSource::JsMetavariable(_) => {
                    // Ignore metavariables
                    return Ok(None);
                }
            };
            let range = import_source.syntax().text_range_with_trivia().into();
            let specifier = import_source
                .inner_string_text()
                .map_err(|error| ScriptParseError::SyntaxError { error, range })?;
            let specifier = specifier.text().to_string();
            Ok(Some(ScriptImport { specifier, range }))
        })
        .filter_map(std::result::Result::transpose)
}

fn find_dynamic_imports(
    script: &ScriptAst,
) -> impl Iterator<Item = Result<ScriptImport, ScriptParseError>> {
    script
        .module
        .syntax()
        .descendants()
        .map(move |node| {
            let range = TextRange::from(node.text_range_with_trivia());
            if let Some(import_call_expr) = biome_js_syntax::JsImportCallExpression::cast(node) {
                // Get the arguments
                let args = import_call_expr
                    .arguments()
                    .map_err(|error| ScriptParseError::SyntaxError { error, range })?
                    .args();
                let args = args
                    .iter()
                    .map(|arg| {
                        let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: args.range().into(),
                        })?;
                        let arg = arg_to_string_literal(&arg, None)?;
                        Result::<_, ScriptParseError>::Ok(arg)
                    })
                    .collect::<Result<Vec<_>, ScriptParseError>>()?;

                // Ensure there's exactly one argument
                let specifier = match &args[..] {
                    [specifier] => specifier.clone(),
                    _ => {
                        return Err(ScriptParseError::UnsupportedFunctionCallArity {
                            range: import_call_expr.range().into(),
                            function: "include",
                            expected_num_args: 1..=1,
                            actual_num_args: args.len(),
                        });
                    }
                };
                Ok(Some(ScriptImport { specifier, range }))
            } else {
                Ok(None)
            }
        })
        .filter_map(std::result::Result::transpose)
}

pub fn find_statics<'a>(
    script: &'a ScriptAst,
    env: &'a HashMap<String, serde_json::Value>,
) -> impl Iterator<Item = Result<ModuleStaticQuery, ScriptParseError>> + 'a {
    script
        .module
        .syntax()
        .descendants()
        .map(move |node| {
            // Get a call expression (e.g. `Brioche.includeFile(...)`)
            let Some(call_expr) = biome_js_syntax::JsCallExpression::cast(node) else {
                return Ok(None);
            };

            let Ok(callee) = call_expr.callee() else {
                return Ok(None);
            };
            let Some(callee) = callee.as_js_static_member_expression() else {
                return Ok(None);
            };

            // Filter down to call expressions accessing a member from `Brioche`
            let Ok(callee_object) = callee.object() else {
                return Ok(None);
            };
            let Some(callee_object) = callee_object.as_js_identifier_expression() else {
                return Ok(None);
            };
            let Ok(callee_object_name) = callee_object.name() else {
                return Ok(None);
            };
            if !callee_object_name.has_name("Brioche") {
                return Ok(None);
            }

            // Filter down to calls to one of the static methods
            let Ok(callee_member) = callee.member() else {
                return Ok(None);
            };
            let Some(callee_member) = callee_member.as_js_name() else {
                return Ok(None);
            };
            let Ok(callee_member_text) = callee_member.value_token() else {
                return Ok(None);
            };
            let callee_member_text = callee_member_text.text_trimmed();

            match callee_member_text {
                "includeFile" => {
                    // Get the arguments
                    let args = call_expr
                        .arguments()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: call_expr.range().into(),
                        })?
                        .args();
                    let args = args
                        .iter()
                        .map(|arg| {
                            let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                                error,
                                range: args.range().into(),
                            })?;
                            let arg = arg_to_string_literal(&arg, Some(env))?;
                            Result::<_, ScriptParseError>::Ok(arg)
                        })
                        .collect::<Result<Vec<_>, ScriptParseError>>()?;

                    // Ensure there's exactly one argument
                    let path = match &args[..] {
                        [path] => path.clone(),
                        _ => {
                            return Err(ScriptParseError::UnsupportedFunctionCallArity {
                                range: call_expr.range().into(),
                                function: "Brioche.includeFile",
                                expected_num_args: 1..=1,
                                actual_num_args: args.len(),
                            });
                        }
                    };

                    let path = RelativePath::new(path);
                    let query = StaticQuery::IncludeFile(path);
                    let range = call_expr.syntax().text_range_with_trivia().into();
                    Ok(Some(ModuleStaticQuery { query, range }))
                }
                "includeDirectory" => {
                    // Get the arguments
                    let args = call_expr
                        .arguments()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: call_expr.range().into(),
                        })?
                        .args();
                    let args = args
                        .iter()
                        .map(|arg| {
                            let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                                error,
                                range: args.range().into(),
                            })?;
                            let arg = arg_to_string_literal(&arg, Some(env))?;
                            Result::<_, ScriptParseError>::Ok(arg)
                        })
                        .collect::<Result<Vec<_>, ScriptParseError>>()?;

                    // Ensure there's exactly one argument
                    let path = match &args[..] {
                        [path] => path.clone(),
                        _ => {
                            return Err(ScriptParseError::UnsupportedFunctionCallArity {
                                range: call_expr.range().into(),
                                function: "Brioche.includeDirectory",
                                expected_num_args: 1..=1,
                                actual_num_args: args.len(),
                            });
                        }
                    };

                    let path = RelativePath::new(path);
                    let query = StaticQuery::IncludeDirectory(path);
                    let range = call_expr.syntax().text_range_with_trivia().into();
                    Ok(Some(ModuleStaticQuery { query, range }))
                }
                "glob" => {
                    // Get the arguments
                    let args = call_expr
                        .arguments()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: call_expr.range().into(),
                        })?
                        .args();
                    let args = args
                        .iter()
                        .map(|arg| {
                            let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                                error,
                                range: args.range().into(),
                            })?;
                            let arg = arg_to_string_literal(&arg, Some(env))?;
                            Result::<_, ScriptParseError>::Ok(arg)
                        })
                        .collect::<Result<Vec<_>, ScriptParseError>>()?;

                    let query = StaticQuery::Glob { patterns: args };
                    let range = call_expr.syntax().text_range_with_trivia().into();
                    Ok(Some(ModuleStaticQuery { query, range }))
                }
                "download" => {
                    // Get the arguments
                    let args = call_expr
                        .arguments()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: call_expr.range().into(),
                        })?
                        .args();
                    let args = args
                        .iter()
                        .map(|arg| {
                            let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                                error,
                                range: args.range().into(),
                            })?;
                            let range = arg.range();
                            let arg = arg_to_string_literal(&arg, Some(env))?;
                            Result::<_, ScriptParseError>::Ok((arg, range))
                        })
                        .collect::<Result<Vec<_>, ScriptParseError>>()?;

                    // Ensure there's exactly one argument
                    let [(url, url_range)] = &args[..] else {
                        return Err(ScriptParseError::UnsupportedFunctionCallArity {
                            range: call_expr.range().into(),
                            function: "Brioche.download",
                            expected_num_args: 1..=1,
                            actual_num_args: args.len(),
                        });
                    };

                    // Parse the URL
                    let url =
                        url.parse()
                            .map_err(|error| ScriptParseError::InvalidFunctionArgument {
                                range: (*url_range).into(),
                                function: "Brioche.download",
                                argument: 0,
                                reason: format!("invalid URL: {error}"),
                            })?;

                    let query = StaticQuery::Download { url };
                    let range = call_expr.syntax().text_range_with_trivia().into();
                    Ok(Some(ModuleStaticQuery { query, range }))
                }
                function @ ("gitRef" | "gitCheckout") => {
                    let function = match function {
                        "gitRef" => "Brioche.gitRef",
                        "gitCheckout" => "Brioche.gitCheckout",
                        _ => unreachable!(),
                    };

                    // Get the arguments
                    let args = call_expr
                        .arguments()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: call_expr.range().into(),
                        })?
                        .args();
                    let args = args
                        .iter()
                        .map(|arg| {
                            let arg = arg.map_err(|error| ScriptParseError::SyntaxError {
                                error,
                                range: args.range().into(),
                            })?;
                            let range = arg.range();
                            let arg = arg_to_json(&arg, Some(env))?;
                            Result::<_, ScriptParseError>::Ok((arg, range))
                        })
                        .collect::<Result<Vec<_>, ScriptParseError>>()?;

                    // Ensure there's exactly one argument
                    let (options, options_range) = match &args[..] {
                        [options] => options.clone(),
                        _ => {
                            return Err(ScriptParseError::UnsupportedFunctionCallArity {
                                range: call_expr.range().into(),
                                function,
                                expected_num_args: 1..=1,
                                actual_num_args: args.len(),
                            });
                        }
                    };

                    // Parse the options
                    let options = serde_json::from_value(options).map_err(|error| {
                        ScriptParseError::InvalidFunctionArgument {
                            range: options_range.into(),
                            function,
                            argument: 0,
                            reason: format!("invalid URL: {error}"),
                        }
                    })?;

                    let query = StaticQuery::GitRef(options);
                    let range = call_expr.syntax().text_range_with_trivia().into();
                    Ok(Some(ModuleStaticQuery { query, range }))
                }
                _ => Ok(None),
            }
        })
        .filter_map(std::result::Result::transpose)
}

#[derive(Debug, Clone)]
pub struct ModuleStaticQuery {
    pub query: StaticQuery,
    pub range: TextRange,
}

fn expression_to_json(
    expr: &biome_js_syntax::AnyJsExpression,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> Result<serde_json::Value, ScriptParseError> {
    use biome_js_syntax::{AnyJsExpression as Expr, AnyJsLiteralExpression as Literal};
    match expr {
        Expr::AnyJsLiteralExpression(literal) => match literal {
            Literal::JsBooleanLiteralExpression(boolean) => {
                let boolean_token =
                    boolean
                        .value_token()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            range: boolean.range().into(),
                            error,
                        })?;
                match boolean_token.text_trimmed() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    _ => Err(ScriptParseError::UnsupportedStaticExpression {
                        range: boolean_token.text_trimmed_range().into(),
                        reason: "invalid boolean".to_string(),
                    }),
                }
            }
            Literal::JsNullLiteralExpression(_) => Ok(serde_json::Value::Null),
            Literal::JsNumberLiteralExpression(number) => {
                let value = number
                    .as_number()
                    .and_then(serde_json::Number::from_f64)
                    .ok_or_else(|| ScriptParseError::UnsupportedStaticExpression {
                        range: number.range().into(),
                        reason: "invalid number".to_string(),
                    })?;
                Ok(serde_json::Value::Number(value))
            }
            Literal::JsStringLiteralExpression(string) => {
                let value =
                    string
                        .inner_string_text()
                        .map_err(|error| ScriptParseError::SyntaxError {
                            error,
                            range: string.range().into(),
                        })?;
                if value.contains('\\') {
                    // TODO: Figure out how to properly unescape the string
                    return Err(ScriptParseError::UnsupportedStaticExpression {
                        range: string.range().into(),
                        reason: "unsupported escape sequence in string literal".to_string(),
                    });
                }

                Ok(serde_json::Value::String(value.text().to_string()))
            }
            _ => Err(ScriptParseError::UnsupportedStaticExpression {
                range: literal.range().into(),
                reason: "unsupported literal value".to_string(),
            }),
        },
        Expr::JsArrayExpression(array) => {
            let values = array
                .elements()
                .iter()
                .map(|element| {
                    let element = element.map_err(|error| ScriptParseError::SyntaxError {
                        error,
                        range: array.range().into(),
                    })?;
                    let biome_js_syntax::AnyJsArrayElement::AnyJsExpression(element) = element
                    else {
                        return Err(ScriptParseError::UnsupportedStaticExpression {
                            range: element.range().into(),
                            reason: "unsupported array element".to_string(),
                        });
                    };
                    let element = expression_to_json(&element, env)?;

                    Result::<_, ScriptParseError>::Ok(element)
                })
                .collect::<Result<Vec<_>, ScriptParseError>>()?;
            Ok(serde_json::Value::Array(values))
        }
        Expr::JsObjectExpression(object) => {
            let members = object
                .members()
                .iter()
                .map(|member| {
                    let member = member.map_err(|error| ScriptParseError::SyntaxError {
                        error,
                        range: object.range().into(),
                    })?;

                    let (key, value) =
                        match member {
                            biome_js_syntax::AnyJsObjectMember::JsPropertyObjectMember(member) => {
                                let key = member.name().map_err(|error| {
                                    ScriptParseError::SyntaxError {
                                        error,
                                        range: member.range().into(),
                                    }
                                })?;
                                let key = match key {
                                biome_js_syntax::AnyJsObjectMemberName::JsComputedMemberName(
                                    computed,
                                ) => {
                                    let key_expr = computed.expression().map_err(|error| {
                                        ScriptParseError::SyntaxError {
                                            error,
                                            range: computed.range().into(),
                                        }
                                    })?;
                                    let key = expression_to_json(&key_expr, env)?;
                                    let serde_json::Value::String(key) = key else {
                                        return Err(
                                            ScriptParseError::UnsupportedStaticExpression {
                                                range: key_expr.range().into(),
                                                reason: "object member name must be a string"
                                                    .to_string(),
                                            },
                                        );
                                    };

                                    key
                                }
                                biome_js_syntax::AnyJsObjectMemberName::JsLiteralMemberName(
                                    name,
                                ) => {
                                    let key = name.name().map_err(|error| {
                                        ScriptParseError::SyntaxError {
                                            error,
                                            range: name.range().into(),
                                        }
                                    })?;
                                    key.text().to_string()
                                }
                                biome_js_syntax::AnyJsObjectMemberName::JsMetavariable(member) => {
                                    return Err(ScriptParseError::UnsupportedStaticExpression {
                                        range: member.range().into(),
                                        reason: "object member name is a metavariable".to_string(),
                                    });
                                }
                            };

                                let value = member.value().map_err(|error| {
                                    ScriptParseError::SyntaxError {
                                        error,
                                        range: member.range().into(),
                                    }
                                })?;
                                let value = expression_to_json(&value, env)?;
                                (key, value)
                            }
                            biome_js_syntax::AnyJsObjectMember::JsShorthandPropertyObjectMember(
                                member,
                            ) => {
                                return Err(ScriptParseError::UnsupportedStaticExpression {
                                    range: member.range().into(),
                                    reason: "shorthand properties are not supported".to_string(),
                                });
                            }
                            _ => {
                                return Err(ScriptParseError::UnsupportedStaticExpression {
                                    range: member.range().into(),
                                    reason: "unsupported object member".to_string(),
                                });
                            }
                        };

                    Result::<_, ScriptParseError>::Ok((key, value))
                })
                .collect::<Result<serde_json::Map<_, _>, ScriptParseError>>()?;

            Ok(serde_json::Value::Object(members))
        }
        Expr::JsTemplateExpression(template) => {
            if let Some(tag) = template.tag() {
                return Err(ScriptParseError::UnsupportedStaticExpression {
                    range: tag.range().into(),
                    reason: "tagged template literals are not supported".to_string(),
                });
            }

            let components = template
                .elements()
                .iter()
                .map(|element| {
                    let value = match element {
                        biome_js_syntax::AnyJsTemplateElement::JsTemplateChunkElement(chunk) => {
                            let chunk_token = chunk.template_chunk_token().map_err(|error| {
                                ScriptParseError::SyntaxError {
                                    error,
                                    range: chunk.range().into(),
                                }
                            })?;
                            let string = chunk_token.text();

                            if string.contains('\\') {
                                return Err(ScriptParseError::UnsupportedStaticExpression {
                                    range: chunk.range().into(),
                                    reason: "template contains unsupported escape sequence"
                                        .to_string(),
                                });
                            }

                            string.to_string()
                        }
                        biome_js_syntax::AnyJsTemplateElement::JsTemplateElement(element) => {
                            let expr = element.expression().map_err(|error| {
                                ScriptParseError::SyntaxError {
                                    error,
                                    range: element.range().into(),
                                }
                            })?;
                            let value = expression_to_json(&expr, env)?;

                            let string = value.as_str().ok_or_else(|| {
                                ScriptParseError::UnsupportedStaticExpression {
                                    range: expr.range().into(),
                                    reason: "template component must be a string".to_string(),
                                }
                            })?;

                            string.to_string()
                        }
                    };

                    Result::<_, ScriptParseError>::Ok(value)
                })
                .collect::<Result<Vec<_>, ScriptParseError>>()?;

            Ok(serde_json::Value::String(components.join("")))
        }
        Expr::JsIdentifierExpression(ident) => {
            let name = ident
                .name()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: ident.range().into(),
                })?;
            let name = name.name().map_err(|error| ScriptParseError::SyntaxError {
                error,
                range: name.range().into(),
            })?;
            let name = name.text();
            let value = env.and_then(|env| env.get(name)).ok_or_else(|| {
                ScriptParseError::UnsupportedStaticExpression {
                    range: ident.range().into(),
                    reason: format!("identifier {name:?} is not recognized in this context"),
                }
            })?;
            Ok(value.clone())
        }
        Expr::JsStaticMemberExpression(expr) => {
            let object_expr = expr
                .object()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let object = expression_to_json(&object_expr, env)?;
            let object = object.as_object().ok_or_else(|| {
                ScriptParseError::UnsupportedStaticExpression {
                    range: object_expr.range().into(),
                    reason: "expected an object".to_string(),
                }
            })?;

            let member = expr
                .member()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let member = match member {
                biome_js_syntax::AnyJsName::JsName(member) => member,
                biome_js_syntax::AnyJsName::JsMetavariable(member) => {
                    return Err(ScriptParseError::UnsupportedStaticExpression {
                        range: member.range().into(),
                        reason: format!("invalid metavariable object member name {member:?}"),
                    });
                }
                biome_js_syntax::AnyJsName::JsPrivateName(member) => {
                    return Err(ScriptParseError::UnsupportedStaticExpression {
                        range: member.range().into(),
                        reason: format!("private member name {member:?} not supported"),
                    });
                }
            };
            let member = member
                .value_token()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let member = member.text();

            let value = object.get(member).ok_or_else(|| {
                ScriptParseError::UnsupportedStaticExpression {
                    range: object_expr.range().into(),
                    reason: format!("member {member:?} not found in object"),
                }
            })?;
            Ok(value.clone())
        }
        Expr::JsComputedMemberExpression(expr) => {
            let object_expr = expr
                .object()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let object = expression_to_json(&object_expr, env)?;

            let member_expr = expr
                .member()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let member = expression_to_json(&member_expr, env)?;

            let value = match (object, member) {
                (serde_json::Value::Object(object), serde_json::Value::String(member)) => object
                    .get(&member)
                    .cloned()
                    .ok_or_else(|| ScriptParseError::UnsupportedStaticExpression {
                        range: member_expr.range().into(),
                        reason: format!("member {member:?} not found in object"),
                    })?,
                (serde_json::Value::Array(array), serde_json::Value::Number(member)) => {
                    let member = member
                        .as_u64()
                        .and_then(|member| usize::try_from(member).ok())
                        .ok_or_else(|| ScriptParseError::UnsupportedStaticExpression {
                            range: member_expr.range().into(),
                            reason: "unsupported array index".to_string(),
                        })?;

                    array.get(member).cloned().ok_or_else(|| {
                        ScriptParseError::UnsupportedStaticExpression {
                            range: member_expr.range().into(),
                            reason: format!("index {member} out of bounds"),
                        }
                    })?
                }
                _ => {
                    return Err(ScriptParseError::UnsupportedStaticExpression {
                        range: member_expr.range().into(),
                        reason: "unsupported index expression".to_string(),
                    });
                }
            };

            Ok(value)
        }
        Expr::JsParenthesizedExpression(expr) => {
            let expr = expr
                .expression()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let value = expression_to_json(&expr, env)?;
            Ok(value)
        }
        Expr::TsAsExpression(expr) => {
            let expr = expr
                .expression()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let value = expression_to_json(&expr, env)?;
            Ok(value)
        }
        Expr::TsNonNullAssertionExpression(expr) => {
            let expr = expr
                .expression()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let value = expression_to_json(&expr, env)?;
            Ok(value)
        }
        Expr::TsSatisfiesExpression(expr) => {
            let expr = expr
                .expression()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let value = expression_to_json(&expr, env)?;
            Ok(value)
        }
        Expr::TsTypeAssertionExpression(expr) => {
            let expr = expr
                .expression()
                .map_err(|error| ScriptParseError::SyntaxError {
                    error,
                    range: expr.range().into(),
                })?;
            let value = expression_to_json(&expr, env)?;
            Ok(value)
        }
        _ => Err(ScriptParseError::UnsupportedStaticExpression {
            range: expr.range().into(),
            reason: "unsupported index expression".to_string(),
        }),
    }
}

fn arg_to_json(
    arg: &biome_js_syntax::AnyJsCallArgument,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> Result<serde_json::Value, ScriptParseError> {
    let arg = arg.as_any_js_expression().ok_or_else(|| {
        ScriptParseError::UnsupportedStaticExpression {
            range: arg.range().into(),
            reason: "spread arguments are not supported".into(),
        }
    })?;
    let arg = expression_to_json(arg, env)?;

    Ok(arg)
}

fn arg_to_string_literal(
    arg: &biome_js_syntax::AnyJsCallArgument,
    env: Option<&HashMap<String, serde_json::Value>>,
) -> Result<String, ScriptParseError> {
    let arg_value = arg_to_json(arg, env)?;
    let arg_value =
        arg_value
            .as_str()
            .ok_or_else(|| ScriptParseError::UnsupportedStaticExpression {
                range: arg.range().into(),
                reason: "expected string argument".into(),
            })?;

    Ok(arg_value.to_string())
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ScriptParseError {
    #[error("syntax error")]
    SyntaxError {
        #[source]
        error: biome_rowan::SyntaxError,
        range: TextRange,
    },
    #[error(
        "'{function}()' can only be called with {expected_num_args:?} arg(s), but got {actual_num_args}"
    )]
    UnsupportedFunctionCallArity {
        range: TextRange,
        function: &'static str,
        expected_num_args: std::ops::RangeInclusive<usize>,
        actual_num_args: usize,
    },

    #[error("argument {argument} for '{function}()' is invalid: {reason}")]
    InvalidFunctionArgument {
        range: TextRange,
        function: &'static str,
        argument: usize,
        reason: String,
    },

    #[error("unsupported static expression: {reason}")]
    UnsupportedStaticExpression { range: TextRange, reason: String },

    #[error("unsupported export: {reason}")]
    UnsupportedExport { range: TextRange, reason: String },
}

impl ScriptParseError {
    #[must_use]
    pub const fn range(&self) -> TextRange {
        match self {
            Self::SyntaxError { range, .. }
            | Self::UnsupportedFunctionCallArity { range, .. }
            | Self::InvalidFunctionArgument { range, .. }
            | Self::UnsupportedStaticExpression { range, .. }
            | Self::UnsupportedExport { range, .. } => *range,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl From<biome_text_size::TextRange> for TextRange {
    fn from(value: biome_text_size::TextRange) -> Self {
        Self {
            start: value.start().into(),
            end: value.end().into(),
        }
    }
}
