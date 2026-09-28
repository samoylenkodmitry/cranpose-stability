//! Item-level parsing. Function bodies are skipped unless they declare nested functions.
use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};

/// Parse items and signatures. Statement syntax inside skipped bodies is not checked.
pub(crate) fn file(source: &str) -> syn::Result<syn::File> {
    match source.trim_start_matches('\u{feff}').parse::<TokenStream>() {
        Ok(tokens) => syn::parse2(strip(tokens)).or_else(|_| syn::parse_file(source)),
        Err(_) => syn::parse_file(source),
    }
}
fn strip(tokens: TokenStream) -> TokenStream {
    let mut body = false;
    tokens
        .into_iter()
        .map(|t| match t {
            TokenTree::Ident(i) => {
                body |= i == "fn";
                TokenTree::Ident(i)
            }
            TokenTree::Punct(p) => {
                body &= p.as_char() != ';';
                TokenTree::Punct(p)
            }
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => {
                let keep = !std::mem::take(&mut body) || nested_fn(&g.stream());
                let mut out = Group::new(
                    Delimiter::Brace,
                    if keep {
                        strip(g.stream())
                    } else {
                        TokenStream::new()
                    },
                );
                out.set_span(g.span());
                TokenTree::Group(out)
            }
            t => t,
        })
        .collect()
}
fn nested_fn(tokens: &TokenStream) -> bool {
    tokens
        .clone()
        .into_iter()
        .any(|t| matches!(t, TokenTree::Ident(i) if i == "fn"))
}
