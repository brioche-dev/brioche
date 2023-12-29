use anyhow::Context as _;
use biome_rowan::{AstNode as _, AstNodeList as _, AstSeparatedList as _};

pub fn analyze_project(
    file: &url::Url,
    contents: &str,
) -> anyhow::Result<super::ProjectDefinition> {
    let parsed = biome_js_parser::parse(
        contents,
        biome_js_syntax::JsFileSource::ts().with_module_kind(biome_js_syntax::ModuleKind::Module),
        biome_js_parser::JsParserOptions::default(),
    )
    .cast::<biome_js_syntax::JsModule>()
    .expect("failed to cast module");

    let module = parsed
        .try_tree()
        .with_context(|| format!("{file}: failed to parse module"))?;

    let project_export = module.items().iter().find_map(|item| {
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

            if id_name.text_trimmed() == "project" {
                Some(declarator)
            } else {
                None
            }
        })
    });

    let project_export = project_export
        .with_context(|| format!("{file}: expected a top-level export named `project`"))?;
    let line = contents[..project_export.syntax().text_range().start().into()]
        .lines()
        .count();
    let file_line = format!("{file}:{line}");
    let project_export_expr = project_export
        .initializer()
        .map(|init| {
            let expr = init.expression().with_context(|| {
                format!("{file_line}: invalid project export: failed to parse expression")
            })?;
            anyhow::Ok(expr.clone())
        })
        .with_context(|| format!("{file_line}: invalid project export: expected assignment like `export const project = {{ ... }}`"))??;

    let json = expression_to_json(&project_export_expr)
        .with_context(|| format!("{file_line}: invalid project export"))?;
    let project_definition = serde_json::from_value(json)
        .with_context(|| format!("{file_line}: invalid project definition"))?;

    Ok(project_definition)
}

fn expression_to_json(
    expr: &biome_js_syntax::AnyJsExpression,
) -> anyhow::Result<serde_json::Value> {
    use biome_js_syntax::{AnyJsExpression as Expr, AnyJsLiteralExpression as Literal};
    match expr {
        Expr::AnyJsLiteralExpression(literal) => match literal {
            Literal::JsBooleanLiteralExpression(boolean) => {
                match boolean.value_token()?.text_trimmed() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    other => {
                        anyhow::bail!("invalid boolean literal: {:?}", other);
                    }
                }
            }
            Literal::JsNullLiteralExpression(_) => Ok(serde_json::Value::Null),
            Literal::JsNumberLiteralExpression(number) => {
                let value = number.as_number().context("invalid number")?;
                let value = serde_json::Number::from_f64(value).context("invalid number")?;
                Ok(serde_json::Value::Number(value))
            }
            Literal::JsStringLiteralExpression(string) => {
                let value = string.inner_string_text().context("invalid string")?;
                Ok(serde_json::Value::String(value.text().to_string()))
            }
            _ => {
                anyhow::bail!("invalid literal value");
            }
        },
        Expr::JsArrayExpression(array) => {
            let mut values = Vec::new();
            for (n, element) in array.elements().iter().enumerate() {
                let element = element.with_context(|| format!("[{n}]: syntax error"))?;
                let element = match element {
                    biome_js_syntax::AnyJsArrayElement::AnyJsExpression(expression) => expression,
                    _ => {
                        anyhow::bail!("[{n}]: unsupported array element");
                    }
                };
                let element = expression_to_json(&element).with_context(|| format!("[{n}]"))?;
                values.push(element);
            }
            Ok(serde_json::Value::Array(values))
        }
        Expr::JsObjectExpression(object) => {
            let mut values = serde_json::Map::new();
            for member in object.members() {
                let member = member.with_context(|| "syntax error".to_string())?;
                let (key, value) = match member {
                    biome_js_syntax::AnyJsObjectMember::JsPropertyObjectMember(member) => {
                        let key = member.name().context("invalid object member name")?;
                        let key = match key {
                            biome_js_syntax::AnyJsObjectMemberName::JsComputedMemberName(
                                computed,
                            ) => {
                                let key_expr = computed
                                    .expression()
                                    .context("invalid object member name")?;
                                let key = expression_to_json(&key_expr)
                                    .context("invalid object member name")?;
                                let serde_json::Value::String(key) = key else {
                                    anyhow::bail!("object member name must be a string");
                                };

                                key
                            }
                            biome_js_syntax::AnyJsObjectMemberName::JsLiteralMemberName(name) => {
                                let key = name.name().context("invalid object member name")?;
                                key.text().to_string()
                            }
                        };

                        let value = member
                            .value()
                            .with_context(|| format!("{key}: syntax error"))?;
                        let value = expression_to_json(&value).with_context(|| key.to_string())?;
                        (key, value)
                    }
                    biome_js_syntax::AnyJsObjectMember::JsShorthandPropertyObjectMember(member) => {
                        let key = member.name().context("invalid object member name")?;
                        let key = key.name().context("invalid object member name")?;
                        let key = key.text();
                        anyhow::bail!("{key}: shorthand properties are not supported");
                    }
                    _ => {
                        anyhow::bail!("unsupported object member");
                    }
                };
                values.insert(key, value);
            }

            Ok(serde_json::Value::Object(values))
        }
        Expr::JsParenthesizedExpression(_) => todo!(),
        Expr::TsAsExpression(_) => todo!(),
        Expr::TsNonNullAssertionExpression(_) => todo!(),
        Expr::TsSatisfiesExpression(_) => todo!(),
        Expr::TsTypeAssertionExpression(_) => todo!(),
        _ => {
            anyhow::bail!("unsupported expression");
        }
    }
}
