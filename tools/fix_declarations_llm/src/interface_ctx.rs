use full_source::CargoPackage;
use quantize_rust_spans::RustItemMap;
use syn::spanned::Spanned;

// Stub helpers (for interface context)
fn stub_item(item: syn::Item, source: &[u8]) -> String {
    let mut source = Vec::from(source);
    match item {
        syn::Item::Fn(f) => {
            let todo = "{ todo!() }".bytes();
            source.splice(f.block.span().byte_range(), todo);
        }
        syn::Item::Impl(impl_block) => {
            let mut pieces = vec![];
            let mut last_end = 0;
            impl_block.items.iter().for_each(|impl_item| {
                if let syn::ImplItem::Fn(method) = impl_item {
                    let byte_range = method.block.span().byte_range();
                    pieces.push(source[last_end..byte_range.start].to_vec());

                    pieces.push(b"{ todo!() }".to_vec());
                    last_end = byte_range.end;
                }
            });
            pieces.push(source[last_end..].to_vec());
            source = pieces.into_iter().flatten().collect();
        }
        _ => {}
    }
    String::from_utf8(source).unwrap()
}

fn stub_declaration(source: &[u8]) -> Option<String> {
    let sstr = str::from_utf8(source).ok()?;
    let Ok(file) = syn::parse_file(sstr) else {
        return Some(sstr.trim_end_matches('\n').to_string());
    };
    file.items
        .into_iter()
        .map(|item| stub_item(item, source))
        .collect::<Vec<_>>()
        .join("\n")
        .into()
}

pub(crate) fn get_interface_ctx(item_map: &RustItemMap, cargo_package: &CargoPackage) -> String {
    item_map
        .items
        .iter()
        .flat_map(|(file_name, irs)| Some((cargo_package.dir.get_file(file_name).ok()?, irs)))
        .flat_map(|(src, item_ranges)| {
            item_ranges
                .iter()
                .flat_map(|r| stub_declaration(&src[r.clone()]))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stub_declaration() {
        assert_eq!(
            Some(
                r"
fn foo() { todo!() }
"
                .into()
            ),
            super::stub_declaration(
                br"
fn foo() { println!() }
"
            )
        );

        assert_eq!(
            Some(
                r"

impl Foo {
    fn foo() { todo!() }

    fn bar() { todo!() }
}

"
                .into()
            ),
            super::stub_declaration(
                br"

impl Foo {
    fn foo() { println!() }

    fn bar() {
      scanf!()
    }
}

"
            )
        );
    }
}
