// Generate a transport-neutral invocation adapter from the MCP tool methods.
// Schemas and handlers remain owned by the existing rmcp router. A browser has
// no rmcp RequestContext/peer: it uses this adapter and the same request queue.
use quote::quote;
use std::{env, fs, path::PathBuf};
use syn::{Expr, ImplItem, Item, PatType, Type};

fn main() {
    let source = "src/plugins/model_api/server.rs";
    println!("cargo:rerun-if-changed={source}");
    let ast = syn::parse_file(&fs::read_to_string(source).unwrap()).unwrap();
    let mut arms = Vec::new();
    let mut names = Vec::new();
    for item in ast.items {
        let Item::Impl(block) = item else { continue };
        for item in block.items {
            let ImplItem::Fn(method) = item else { continue };
            let Some(attr) = method.attrs.iter().find(|a| a.path().is_ident("tool")) else {
                continue;
            };
            let mut name = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    name = Some(value.value());
                } else {
                    let _: Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .unwrap();
            let name = name.expect("every MCP tool has an explicit wire name");
            names.push(name.clone());
            let ident = method.sig.ident;
            let mut parameter_type = None;
            let mut context = false;
            for input in method.sig.inputs {
                let syn::FnArg::Typed(PatType { ty, .. }) = input else {
                    continue;
                };
                if let Type::Path(path) = *ty {
                    let last = path.path.segments.last().unwrap();
                    if last.ident == "Parameters" {
                        let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
                            panic!("Parameters<T>")
                        };
                        parameter_type = Some(args.args.first().unwrap().clone());
                    } else if last.ident == "RequestContext" {
                        context = true;
                    } else {
                        panic!("unsupported MCP argument: {ident}");
                    }
                } else {
                    panic!("unsupported argument shape: {ident}");
                }
            }
            let call = match (parameter_type, context) {
                (Some(ty), false) => {
                    quote!(self.#ident(Parameters(serde_json::from_value::<#ty>(arguments).map_err(|e| McpError::invalid_params(e.to_string(),None))?)).await)
                }
                (None, false) => quote!(self.#ident().await),
                (Some(ty), true) if name == "set_session_profile" => {
                    quote!(self.set_session_profile_without_peer(serde_json::from_value::<#ty>(arguments).map_err(|e| McpError::invalid_params(e.to_string(),None))?).await)
                }
                _ => panic!("tool needs explicit transport adapter: {ident}"),
            };
            arms.push(quote!(#name => #call));
        }
    }
    assert!(arms.len() > 100, "unexpectedly incomplete MCP tool source");
    let output = quote! {
        #[cfg(test)]
        const WITHOUT_PEER_TOOL_NAMES:&[&str]=&[#(#names),*];
        impl ModelApiServer {
            pub(crate) async fn call_without_peer(&self,name:&str,arguments:Value) -> Result<CallToolResult,McpError> {
                self.tool_call_allowed(name)?;
                let guarded = tool_requires_authored_base(name);
                REQUIRES_AUTHORED_BASE.scope(guarded,async {
                    match name { #(#arms,)* _ => Err(McpError::invalid_params(format!("Unknown MCP tool: {name}"),None)) }
                }).await
            }
        }
    };
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("mcp_without_peer.rs"),
        output.to_string(),
    )
    .unwrap();
}
