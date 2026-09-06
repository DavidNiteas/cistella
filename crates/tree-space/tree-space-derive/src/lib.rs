//! `#[derive(TreeNode)]` — compile-time field registry + xpath navigation.
//!
//! For a struct annotated with this derive, the macro emits an
//! `impl tree_space::tree::TreeNode` that carries:
//!
//! - a static `NodeMeta` table (field names, multiplicities, targets),
//! - a `get(&self, XPath)` navigation entry point,
//! - a `leaf_refs(&self)` collector over every leaf reference in the subtree.
//!
//! The generated code references `::tree_space::` items; the `tree-space`
//! crate must therefore be a dependency of any crate that uses this derive.

use proc_macro::TokenStream;
use quote::{ToTokens, quote};
use syn::{
    Data, DataStruct, DeriveInput, Field, Fields, GenericArgument, PathArguments, Type, TypePath,
    parse_macro_input,
};

/// The single supported field attribute: `#[tree_space(ephemeral)]`.
const EPHEMERAL_ATTR: &str = "ephemeral";

/// The derive entry point.
#[proc_macro_derive(TreeNode, attributes(tree_space))]
pub fn derive_tree_node(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_impl(&input) {
        Ok(stream) => stream.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// The `TreeInstance` derive: generates `TreeInstance::new_empty()` as a
/// fabric of `Default` initializers (every field must implement `Default`).
#[proc_macro_derive(TreeInstance, attributes(tree_space))]
pub fn derive_tree_instance(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match tree_instance_impl(&input) {
        Ok(stream) => stream.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn tree_instance_impl(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[derive(TreeInstance)] is only supported on structs",
            ));
        }
    };
    let field_names = match &data.fields {
        Fields::Named(named) => named
            .named
            .iter()
            .filter_map(|field| field.ident.clone())
            .collect::<Vec<_>>(),
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[derive(TreeInstance)] requires a named-field struct",
            ));
        }
    };
    if field_names.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "cannot derive TreeInstance on a struct without fields",
        ));
    }
    let fields = field_names.iter().map(|ident| {
        quote! { #ident: ::core::default::Default::default() }
    });
    Ok(quote! {
        impl ::tree_space::TreeInstance for #name {
            fn new_empty() -> Self {
                Self { #(#fields),* }
            }
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MultiplicityKind {
    Single,
    Optional,
    Sequence,
    Map,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TargetKind {
    Node,
    Block,
    Inline,
}

#[derive(Clone, Debug)]
struct ParsedField {
    name: String,
    ident: syn::Ident,
    multiplicity: MultiplicityKind,
    target_kind: TargetKind,
    target_tokens: proc_macro2::TokenStream,
    leaf_ty: syn::Type,
    block_kind: Option<proc_macro2::TokenStream>,
    map_key_bytes: Option<bool>,
    /// Whether the field carries the `#[tree_space(ephemeral)]` annotation.
    ephemeral: bool,
}

fn derive_impl(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[derive(TreeNode)] is only supported on structs",
            ));
        }
    };
    if !matches!(data.fields, Fields::Named(_)) {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(TreeNode)] requires a named-field struct",
        ));
    }

    let fields = collect_fields(data)?;
    let mut field_inits = Vec::new();
    let mut get_arms = Vec::new();
    let mut leaf_ref_blocks = Vec::new();
    let mut set_arms = Vec::new();
    let mut best_effort_arms = Vec::new();
    for field in &fields {
        let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
        let mult = multiplicity_tokens(&field.multiplicity);
        let target = &field.target_tokens;
        let ephemeral_lit = field.ephemeral;
        field_inits.push(quote! {
            ::tree_space::FieldMeta {
                name: #name_lit,
                multiplicity: #mult,
                target: #target,
                ephemeral: #ephemeral_lit,
            }
        });
        get_arms.push(get_arm(field));
        leaf_ref_blocks.push(leaf_ref_block(field));
        set_arms.push(set_arm(field));
        best_effort_arms.push(best_effort_arm(field));
    }

    let type_name = name.to_string();
    let expanded = quote! {
        impl ::tree_space::TreeNodeMeta for #name {
            const META: &'static ::tree_space::NodeMeta = &::tree_space::NodeMeta {
                type_name: #type_name,
                fields: &[ #(#field_inits),* ],
            };
        }

        impl ::tree_space::TreeNode for #name {
            fn get(&self, xpath: &::tree_space::xpath::XPath)
                -> ::tree_space::Result<::tree_space::tree::AccessOut>
            {
                let (step, rest) = xpath.take_first().ok_or_else(|| {
                    ::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "empty xpath addresses no field",
                    )
                })?;
                match step {
                    #(#get_arms)*
                    _ => Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "unknown field in xpath",
                    )),
                }
            }

            fn get_best_effort(&self, xpath: &::tree_space::xpath::XPath)
                -> (::tree_space::tree::AccessOut, ::tree_space::xpath::XPath)
            {
                let Some((step, rest)) = xpath.take_first() else {
                    return (
                        ::tree_space::tree::AccessOut::Node(Box::new(self.clone())),
                        ::tree_space::xpath::XPath::root(),
                    );
                };
                match step {
                    #(#best_effort_arms)*
                    _ => (
                        ::tree_space::tree::AccessOut::Node(Box::new(self.clone())),
                        xpath.clone(),
                    ),
                }
            }

            fn set(&mut self, xpath: &::tree_space::xpath::XPath, v: ::tree_space::Slot)
                -> ::tree_space::Result<()>
            {
                let (step, rest) = xpath.take_first().ok_or_else(|| {
                    ::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "empty xpath addresses no field",
                    )
                })?;
                match step {
                    #(#set_arms)*
                    _ => Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "unknown field in xpath",
                    )),
                }
            }

            fn leaf_refs(&self) -> Vec<(::tree_space::xpath::XPath, ::tree_space::RefId)> {
                let mut out = Vec::new();
                #(#leaf_ref_blocks)*
                out
            }
        }
    };
    if std::env::var_os("TS_DERIVE_DEBUG").is_some() {
        eprintln!("=== expanded for {} ===\n{}", name, expanded);
    }
    Ok(expanded)
}

fn multiplicity_tokens(kind: &MultiplicityKind) -> proc_macro2::TokenStream {
    match kind {
        MultiplicityKind::Single => quote! { ::tree_space::Multiplicity::Single },
        MultiplicityKind::Optional => quote! { ::tree_space::Multiplicity::Optional },
        MultiplicityKind::Sequence => quote! { ::tree_space::Multiplicity::Sequence },
        MultiplicityKind::Map => quote! { ::tree_space::Multiplicity::Map },
    }
}

fn err_tokens(message: &str) -> proc_macro2::TokenStream {
    let message = syn::LitStr::new(message, proc_macro2::Span::call_site());
    quote! {
        ::tree_space::TreeSpaceError::new(::tree_space::ErrorCode::XpathUnreachable, #message)
    }
}

/// Generates the `get_best_effort` match arm for one field.
///
/// Best-effort navigation never fails: on an unreachable/absent step it
/// returns the deepest reachable value (`self` as a node) together with the
/// unconsumed xpath suffix. A reached leaf always stops there, returning the
/// leaf and any sub-steps the caller asked past it.
fn best_effort_arm(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let fallback = quote! {
        (
            ::tree_space::tree::AccessOut::Node(Box::new(self.clone())),
            xpath.clone(),
        )
    };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                if rest.is_root() {
                    (
                        ::tree_space::tree::AccessOut::Node(Box::new(self.#ident.clone())),
                        ::tree_space::xpath::XPath::root(),
                    )
                } else {
                    self.#ident.get_best_effort(&rest)
                }
            }
        },
        (TargetKind::Node, MultiplicityKind::Optional) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => match &self.#ident {
                Some(node) => {
                    if rest.is_root() {
                        (
                            ::tree_space::tree::AccessOut::Node(Box::new(node.clone())),
                            ::tree_space::xpath::XPath::root(),
                        )
                    } else {
                        node.get_best_effort(&rest)
                    }
                }
                None => #fallback,
            },
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                let Some((istep, rest2)) = rest.take_first() else {
                    return #fallback;
                };
                match istep {
                    ::tree_space::xpath::Step::Index(i) => match self.#ident.get(i) {
                        Some(node) => {
                            if rest2.is_root() {
                                (
                                    ::tree_space::tree::AccessOut::Node(Box::new(node.clone())),
                                    ::tree_space::xpath::XPath::root(),
                                )
                            } else {
                                node.get_best_effort(&rest2)
                            }
                        }
                        None => #fallback,
                    },
                    _ => #fallback,
                }
            }
        },
        (TargetKind::Node, MultiplicityKind::Map) => {
            let lookup = map_lookup_expr(field);
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let Some((kstep, rest2)) = rest.take_first() else {
                        return #fallback;
                    };
                    match kstep {
                        ::tree_space::xpath::Step::Field(k) => match #lookup {
                            Some(node) => {
                                if rest2.is_root() {
                                    (
                                        ::tree_space::tree::AccessOut::Node(Box::new(node.clone())),
                                        ::tree_space::xpath::XPath::root(),
                                    )
                                } else {
                                    node.get_best_effort(&rest2)
                                }
                            }
                            None => #fallback,
                        },
                        _ => #fallback,
                    }
                }
            }
        }
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Single) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                (::tree_space::tree::LeafToOut::to_out(&self.#ident), rest)
            }
        },
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Optional) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => match &self.#ident {
                Some(leaf) => (::tree_space::tree::LeafToOut::to_out(leaf), rest),
                None => #fallback,
            },
        },
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Sequence) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                let Some((istep, rest2)) = rest.take_first() else {
                    return #fallback;
                };
                match istep {
                    ::tree_space::xpath::Step::Index(i) => match self.#ident.get(i) {
                        Some(leaf) => (::tree_space::tree::LeafToOut::to_out(leaf), rest2),
                        None => #fallback,
                    },
                    _ => #fallback,
                }
            }
        },
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Map) => {
            let lookup = map_lookup_expr(field);
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let Some((kstep, rest2)) = rest.take_first() else {
                        return #fallback;
                    };
                    match kstep {
                        ::tree_space::xpath::Step::Field(k) => match #lookup {
                            Some(leaf) => (::tree_space::tree::LeafToOut::to_out(leaf), rest2),
                            None => #fallback,
                        },
                        _ => #fallback,
                    }
                }
            }
        }
    }
}

/// Generates the `set` match arm for one field.
///
/// Write support: a single/optional inline field stores the incoming scalar
/// (`Slot::Inline(Value)`); a single/optional node field recurses into the
/// sub-node. Typed block fields and container fields are not writable through
/// the weak bridge (typed blocks have no slot storage) and return a clear
/// error; dynamic nodes carry full `Slot` semantics on their own path.
fn set_arm(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                if rest.is_root() {
                    Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "cannot replace a node via the weak set bridge",
                    ))
                } else {
                    self.#ident.set(&rest, v)
                }
            }
        },
        (TargetKind::Node, MultiplicityKind::Optional) => {
            let e = err_tokens("optional node is absent");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    if rest.is_root() {
                        Err(::tree_space::TreeSpaceError::new(
                            ::tree_space::ErrorCode::XpathUnreachable,
                            "cannot replace a node via the weak set bridge",
                        ))
                    } else if let Some(node) = &mut self.#ident {
                        node.set(&rest, v)
                    } else {
                        Err(#e)
                    }
                }
            }
        }
        (TargetKind::Inline, MultiplicityKind::Single) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                if !rest.is_root() {
                    Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "inline leaf has no child steps",
                    ))
                } else {
                    match v {
                        ::tree_space::tree::Slot::Inline(value) => {
                            self.#ident = value;
                            Ok(())
                        }
                        _ => Err(::tree_space::TreeSpaceError::new(
                            ::tree_space::ErrorCode::XpathUnreachable,
                            "inline field cannot hold a block reference",
                        )),
                    }
                }
            }
        },
        (TargetKind::Inline, MultiplicityKind::Optional) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                if !rest.is_root() {
                    Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::XpathUnreachable,
                        "inline leaf has no child steps",
                    ))
                } else {
                    match v {
                        ::tree_space::tree::Slot::Inline(value) => {
                            self.#ident = Some(value);
                            Ok(())
                        }
                        _ => Err(::tree_space::TreeSpaceError::new(
                            ::tree_space::ErrorCode::XpathUnreachable,
                            "inline field cannot hold a block reference",
                        )),
                    }
                }
            }
        },
        (TargetKind::Block, _) => {
            let e = err_tokens(
                "typed block field is not writable through the weak set bridge; use strong typing or a dynamic node",
            );
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    Err(#e)
                }
            }
        }
        _ => {
            let e = err_tokens("field is not writable through the weak set bridge");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    Err(#e)
                }
            }
        }
    }
}

/// Generates the match arm of `get` for one field.
fn get_arm(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                if rest.is_root() {
                    Ok(::tree_space::tree::AccessOut::Node(Box::new(self.#ident.clone())))
                } else {
                    self.#ident.get(&rest)
                }
            }
        },
        (TargetKind::Node, MultiplicityKind::Optional) => {
            let e = err_tokens("optional node is absent");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    match &self.#ident {
                        Some(node) => {
                            if rest.is_root() {
                                Ok(::tree_space::tree::AccessOut::Node(Box::new(node.clone())))
                            } else {
                                node.get(&rest)
                            }
                        }
                        None => Err(#e),
                    }
                }
            }
        }
        (TargetKind::Node, MultiplicityKind::Sequence) => {
            let e0 = err_tokens("sequence field requires an index step");
            let e1 = err_tokens("sequence index out of range");
            let e2 = err_tokens("sequence expects [i]");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let (istep, rest2) = rest.take_first().ok_or_else(|| #e0)?;
                    match istep {
                        ::tree_space::xpath::Step::Index(i) => {
                            let node = self.#ident.get(i).ok_or_else(|| #e1)?;
                            if rest2.is_root() {
                                Ok(::tree_space::tree::AccessOut::Node(Box::new(node.clone())))
                            } else {
                                node.get(&rest2)
                            }
                        }
                        _ => Err(#e2),
                    }
                }
            }
        }
        (TargetKind::Node, MultiplicityKind::Map) => {
            let e0 = err_tokens("map field requires a key step");
            let e1 = err_tokens("map key not found");
            let e2 = err_tokens("map expects a field step");
            let lookup = map_lookup_expr(field);
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let (kstep, rest2) = rest.take_first().ok_or_else(|| #e0)?;
                    match kstep {
                        ::tree_space::xpath::Step::Field(k) => {
                            let node = #lookup.ok_or_else(|| #e1)?;
                            if rest2.is_root() {
                                Ok(::tree_space::tree::AccessOut::Node(Box::new(node.clone())))
                            } else {
                                node.get(&rest2)
                            }
                        }
                        _ => Err(#e2),
                    }
                }
            }
        }
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Single) => {
            let e0 = err_tokens("leaf has no child steps");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    if rest.is_root() {
                        Ok(::tree_space::tree::LeafToOut::to_out(&self.#ident))
                    } else {
                        Err(#e0)
                    }
                }
            }
        }
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Optional) => {
            let e0 = err_tokens("leaf has no child steps");
            let e1 = err_tokens("optional leaf is absent");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    match &self.#ident {
                        Some(leaf) => {
                            if rest.is_root() {
                                Ok(::tree_space::tree::LeafToOut::to_out(leaf))
                            } else {
                                Err(#e0)
                            }
                        }
                        None => Err(#e1),
                    }
                }
            }
        }
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Sequence) => {
            let e0 = err_tokens("leaf sequence requires an index step");
            let e1 = err_tokens("leaf sequence index out of range");
            let e2 = err_tokens("leaf sequence expects [i]");
            let e3 = err_tokens("leaf has no child steps");
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let (istep, rest2) = rest.take_first().ok_or_else(|| #e0)?;
                    match istep {
                        ::tree_space::xpath::Step::Index(i) => {
                            let leaf = self.#ident.get(i).ok_or_else(|| #e1)?;
                            if rest2.is_root() {
                                Ok(::tree_space::tree::LeafToOut::to_out(leaf))
                            } else {
                                Err(#e3)
                            }
                        }
                        _ => Err(#e2),
                    }
                }
            }
        }
        (TargetKind::Block | TargetKind::Inline, MultiplicityKind::Map) => {
            let e0 = err_tokens("leaf map requires a key step");
            let e1 = err_tokens("leaf map key not found");
            let e2 = err_tokens("leaf map expects a field step");
            let e3 = err_tokens("leaf has no child steps");
            let lookup = map_lookup_expr(field);
            quote! {
                ::tree_space::xpath::Step::Field(n) if n == #name_lit => {
                    let (kstep, rest2) = rest.take_first().ok_or_else(|| #e0)?;
                    match kstep {
                        ::tree_space::xpath::Step::Field(k) => {
                            let leaf = #lookup.ok_or_else(|| #e1)?;
                            if rest2.is_root() {
                                Ok(::tree_space::tree::LeafToOut::to_out(leaf))
                            } else {
                                Err(#e3)
                            }
                        }
                        _ => Err(#e2),
                    }
                }
            }
        }
    }
}

/// Generates the leaf-collection block for one field.
fn leaf_ref_block(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let prefix_single = quote! { ::tree_space::xpath::XPath::root().field(#name_lit) };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Inline, _) => quote! {},
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            out.extend(
                self.#ident.leaf_refs().into_iter().map(|(rel, id)| {
                    (#prefix_single.join(&rel), id)
                }),
            );
        },
        (TargetKind::Node, MultiplicityKind::Optional) => quote! {
            if let Some(node) = &self.#ident {
                out.extend(
                    node.leaf_refs().into_iter().map(|(rel, id)| {
                        (#prefix_single.join(&rel), id)
                    }),
                );
            }
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {
            for (i, node) in self.#ident.iter().enumerate() {
                let prefix = ::tree_space::xpath::XPath::root().field(#name_lit).index(i);
                out.extend(
                    node.leaf_refs().into_iter().map(|(rel, id)| (prefix.clone().join(&rel), id)),
                );
            }
        },
        (TargetKind::Node, MultiplicityKind::Map) => {
            let entry = map_entry_prefix(field);
            quote! {
                for (k, node) in &self.#ident {
                    let prefix = #entry;
                    out.extend(
                        node.leaf_refs().into_iter().map(|(rel, id)| (prefix.clone().join(&rel), id)),
                    );
                }
            }
        }
        (TargetKind::Block, MultiplicityKind::Single) => quote! {
            out.push((
                #prefix_single,
                ::tree_space::tree::LeafRefOf::leaf_ref(&self.#ident),
            ));
        },
        (TargetKind::Block, MultiplicityKind::Optional) => quote! {
            if let Some(leaf) = &self.#ident {
                out.push((
                    #prefix_single,
                    ::tree_space::tree::LeafRefOf::leaf_ref(leaf),
                ));
            }
        },
        (TargetKind::Block, MultiplicityKind::Sequence) => quote! {
            for (i, leaf) in self.#ident.iter().enumerate() {
                out.push((
                    ::tree_space::xpath::XPath::root().field(#name_lit).index(i),
                    ::tree_space::tree::LeafRefOf::leaf_ref(leaf),
                ));
            }
        },
        (TargetKind::Block, MultiplicityKind::Map) => {
            let entry = map_entry_prefix(field);
            quote! {
                for (k, leaf) in &self.#ident {
                    out.push((
                        #entry,
                        ::tree_space::tree::LeafRefOf::leaf_ref(leaf),
                    ));
                }
            }
        }
    }
}

/// Generates the map-entry xpath prefix (`field(map).field(<key>)`) for one
/// map entry, hex-encoding `[u8; 16]` keys into their 32-char field-name form.
fn map_entry_prefix(field: &ParsedField) -> proc_macro2::TokenStream {
    let name_lit = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    if field.map_key_bytes == Some(true) {
        quote! {
            ::tree_space::xpath::XPath::root().field(#name_lit).field(::tree_space::layout::tb::hex16(k))
        }
    } else {
        quote! {
            ::tree_space::xpath::XPath::root().field(#name_lit).field(k.clone())
        }
    }
}

/// Generates the map-entry lookup for a `Step::Field(k)` key step, producing an
/// `Option<&target>` (bytes16 hex field names are re-decoded before lookup).
fn map_lookup_expr(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    if field.map_key_bytes == Some(true) {
        quote! {
            ::tree_space::layout::tb::parse_hex16(&k).and_then(|key| self.#ident.get(&key))
        }
    } else {
        quote! { self.#ident.get(&k) }
    }
}

fn collect_fields(data: &DataStruct) -> syn::Result<Vec<ParsedField>> {
    let mut result = Vec::new();
    if let Fields::Named(named) = &data.fields {
        for field in &named.named {
            result.push(parse_field(field)?);
        }
    }
    Ok(result)
}

/// The `TreeCodec` derive: generates the A-2 typed encode/decode impls.
///
/// This produces `tree_space::tree::codec::EncodeTree` (typed instance →
/// tree-image children) and `tree_space::tree::codec::DecodeTree` (tree-image
/// children + bucket → typed instance, materializing block leaves).
#[proc_macro_derive(TreeCodec, attributes(tree_space))]
pub fn derive_tree_codec(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match tree_codec_impl(&input) {
        Ok(stream) => stream.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn tree_codec_impl(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let data = match &input.data {
        Data::Struct(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "#[derive(TreeCodec)] is only supported on structs",
            ));
        }
    };
    if !matches!(data.fields, Fields::Named(_)) {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(TreeCodec)] requires a named-field struct",
        ));
    }
    let fields = collect_fields(data)?;
    let encode_blocks = fields.iter().map(codec_encode_block);
    let decode_blocks = fields.iter().map(codec_decode_block);
    let check_blocks = fields.iter().map(codec_check_block);
    let idents = fields
        .iter()
        .map(|field| field.ident.clone())
        .collect::<Vec<_>>();
    let bindings = idents
        .iter()
        .map(|ident| quote! { #ident: #ident })
        .collect::<Vec<_>>();
    Ok(quote! {
        impl ::tree_space::tree::codec::EncodeTree for #name {
            fn tree_children(
                &self,
            ) -> ::tree_space::Result<std::vec::Vec<::tree_space::tree::codec::ImageField>>
            {
                let mut out = std::vec::Vec::new();
                #(#encode_blocks)*
                Ok(out)
            }
        }

        impl ::tree_space::tree::codec::DecodeTree for #name {
            fn from_image_children(
                children: &[::tree_space::tree::codec::ImageField],
                bucket: &::tree_space::Bucket,
            ) -> ::tree_space::Result<Self> {
                let _ = bucket;
                let mut map = ::tree_space::tree::codec::named_child_map(children)?;
                #(#decode_blocks)*
                if !map.is_empty() {
                    return Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::SchemaMismatch,
                        "tree image has children that the typed node does not declare",
                    ));
                }
                Ok(Self { #(#bindings),* })
            }

            fn check_image_children(
                children: &[::tree_space::tree::codec::ImageField],
                bucket: &::tree_space::Bucket,
            ) -> ::tree_space::Result<()> {
                let _ = bucket;
                let mut map = ::tree_space::tree::codec::named_child_map(children)?;
                #(#check_blocks)*
                if !map.is_empty() {
                    return Err(::tree_space::TreeSpaceError::new(
                        ::tree_space::ErrorCode::SchemaMismatch,
                        "tree image has children that the typed node does not declare",
                    ));
                }
                Ok(())
            }
        }
    })
}

/// Generates the encode block for one field (appends image children to `out`).
fn codec_encode_block(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let named = quote! { ::tree_space::tree::codec::named_field };
    let positioned = quote! { ::tree_space::tree::codec::positioned_field };
    let inline = quote! { ::tree_space::tree::codec::ImageContent::Inline };
    let children_variant = quote! { ::tree_space::tree::codec::ImageContent::Node };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Inline, MultiplicityKind::Single) => quote! {
            out.push(#named(#name, #inline(self.#ident.clone())));
        },
        (TargetKind::Inline, MultiplicityKind::Optional) => quote! {
            if let Some(value) = &self.#ident {
                out.push(#named(#name, #inline(value.clone())));
            }
        },
        (TargetKind::Inline, MultiplicityKind::Sequence) => quote! {
            let mut children = std::vec::Vec::new();
            for value in &self.#ident {
                children.push(#positioned(#inline(value.clone())));
            }
            out.push(#named(#name, #children_variant(children)));
        },
        (TargetKind::Inline, MultiplicityKind::Map) => codec_encode_map(
            field,
            &quote! {
                #inline(value.clone())
            },
        ),
        (TargetKind::Block, MultiplicityKind::Optional) => quote! {
            if let Some(leaf) = &self.#ident {
                out.push(#named(#name, ::tree_space::tree::codec::ImageContent::Ref(
                    ::tree_space::Block::ref_id(leaf),
                )));
            }
        },
        (TargetKind::Block, MultiplicityKind::Single) => quote! {
            out.push(#named(#name, ::tree_space::tree::codec::ImageContent::Ref(
                ::tree_space::Block::ref_id(&self.#ident),
            )));
        },
        (TargetKind::Block, MultiplicityKind::Sequence) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {
                let mut children = std::vec::Vec::new();
                for leaf in &self.#ident {
                    children.push(#positioned(::tree_space::tree::codec::ImageContent::ChunkEntry(
                        ::tree_space::Block::ref_id(leaf),
                    )));
                }
                out.push(#named(#name, ::tree_space::tree::codec::ImageContent::ChunkGroup {
                    kind: #kind,
                    children,
                }));
            }
        }
        (TargetKind::Block, MultiplicityKind::Map) => codec_encode_map(
            field,
            &quote! {
                ::tree_space::tree::codec::ImageContent::Ref(::tree_space::Block::ref_id(value))
            },
        ),
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            out.push(#named(#name, ::tree_space::tree::codec::ImageContent::Node(
                ::tree_space::tree::codec::EncodeTree::tree_children(&self.#ident)?,
            )));
        },
        (TargetKind::Node, MultiplicityKind::Optional) => quote! {
            if let Some(node) = &self.#ident {
                out.push(#named(#name, ::tree_space::tree::codec::ImageContent::Node(
                    ::tree_space::tree::codec::EncodeTree::tree_children(node)?,
                )));
            }
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {
            let mut children = std::vec::Vec::new();
            for node in &self.#ident {
                children.push(#positioned(::tree_space::tree::codec::ImageContent::Node(
                    ::tree_space::tree::codec::EncodeTree::tree_children(node)?,
                )));
            }
            out.push(#named(#name, #children_variant(children)));
        },
        (TargetKind::Node, MultiplicityKind::Map) => codec_encode_map(
            field,
            &quote! {
                ::tree_space::tree::codec::ImageContent::Node(
                    ::tree_space::tree::codec::EncodeTree::tree_children(value)?,
                )
            },
        ),
    }
}

/// Generates a container-encode block over a `BTreeMap` field (named or keyed).
fn codec_encode_map(
    field: &ParsedField,
    content: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let named = quote! { ::tree_space::tree::codec::named_field };
    let keyed = quote! { ::tree_space::tree::codec::keyed_field };
    let children_variant = quote! { ::tree_space::tree::codec::ImageContent::Node };
    if field.map_key_bytes == Some(true) {
        quote! {
            let mut children = std::vec::Vec::new();
            for (key, value) in &self.#ident {
                children.push(#keyed(*key, #content));
            }
            out.push(#named(#name, #children_variant(children)));
        }
    } else {
        quote! {
            let mut children = std::vec::Vec::new();
            for (key, value) in &self.#ident {
                children.push(#named(key, #content));
            }
            out.push(#named(#name, #children_variant(children)));
        }
    }
}

/// Generates the decode block for one field (binds a `let #ident = ...;`).
fn codec_decode_block(field: &ParsedField) -> proc_macro2::TokenStream {
    if field.ephemeral {
        return codec_decode_ephemeral_block(field);
    }
    let ident = &field.ident;
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let leaf = &field.leaf_ty;
    let required = quote! { ::tree_space::tree::codec::require_child(&mut map, #name)? };
    let optional = quote! { ::tree_space::tree::codec::take_child(&mut map, #name)? };
    let decode_tree = quote! { ::tree_space::tree::codec::DecodeTree::from_image_children };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Inline, MultiplicityKind::Single) => quote! {
            let #ident = ::tree_space::tree::codec::expect_inline(#required, #name)?.clone();
        },
        (TargetKind::Inline, MultiplicityKind::Optional) => quote! {
            let #ident = match #optional {
                Some(child) => Some(::tree_space::tree::codec::expect_inline(child, #name)?.clone()),
                None => None,
            };
        },
        (TargetKind::Inline, MultiplicityKind::Sequence) => quote! {
            let #ident = {
                let child = #required;
                let kids = ::tree_space::tree::codec::expect_node(child, #name)?;
                ::tree_space::tree::codec::check_positioned(kids, #name)?;
                let mut values = std::vec::Vec::new();
                for kid in kids {
                    values.push(::tree_space::tree::codec::expect_inline(kid, #name)?.clone());
                }
                values
            };
        },
        (TargetKind::Inline, MultiplicityKind::Map) => codec_decode_map(
            field,
            &quote! {
                ::tree_space::tree::codec::expect_inline_of(content, #name)?.clone()
            },
        ),
        (TargetKind::Block, MultiplicityKind::Single) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {
                let #ident = {
                    let id = ::tree_space::tree::codec::expect_ref(#required, #name)?;
                    ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?
                };
            }
        }
        (TargetKind::Block, MultiplicityKind::Optional) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {
                let #ident = match #optional {
                    Some(child) => {
                        let id = ::tree_space::tree::codec::expect_ref(child, #name)?;
                        Some(::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?)
                    }
                    None => None,
                };
            }
        }
        (TargetKind::Block, MultiplicityKind::Sequence) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {
                let #ident = {
                    let child = #required;
                    let (kind, kids) = ::tree_space::tree::codec::expect_chunk_group(child, #name)?;
                    ::tree_space::tree::codec::check_chunk_kind(kind, &#kind, #name)?;
                    let ids = ::tree_space::tree::codec::chunk_refs(kind, kids, #name)?;
                    let mut values = std::vec::Vec::new();
                    for id in ids {
                        values.push(::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, kind, bucket)?);
                    }
                    values
                };
            }
        }
        (TargetKind::Block, MultiplicityKind::Map) => {
            let kind = field.block_kind.clone().expect("block field kind");
            codec_decode_block_map(
                field,
                &quote! {{
                    let id = ::tree_space::tree::codec::expect_ref_of(content, #name)?;
                    ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?
                }},
            )
        }
        (TargetKind::Node, MultiplicityKind::Single) => quote! {
            let #ident = {
                let child = #required;
                #decode_tree(::tree_space::tree::codec::expect_node(child, #name)?, bucket)?
            };
        },
        (TargetKind::Node, MultiplicityKind::Optional) => quote! {
            let #ident = match #optional {
                Some(child) => Some(#decode_tree(
                    ::tree_space::tree::codec::expect_node(child, #name)?,
                    bucket,
                )?),
                None => None,
            };
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {
            let #ident = {
                let child = #required;
                let kids = ::tree_space::tree::codec::expect_node(child, #name)?;
                ::tree_space::tree::codec::check_positioned(kids, #name)?;
                let mut values = std::vec::Vec::new();
                for kid in kids {
                    values.push(#decode_tree(
                        ::tree_space::tree::codec::expect_node(kid, #name)?,
                        bucket,
                    )?);
                }
                values
            };
        },
        (TargetKind::Node, MultiplicityKind::Map) => codec_decode_map(
            field,
            &quote! {{
                let kids = ::tree_space::tree::codec::expect_node_of(content, #name)?;
                #decode_tree(kids, bucket)?
            }},
        ),
    }
}

/// Generates a container-decode block over a map field (named or keyed).
fn codec_decode_map(
    field: &ParsedField,
    content: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let child = quote! { ::tree_space::tree::codec::require_child(&mut map, #name)? };
    let value = codec_decode_map_value(field, &child, content);
    quote! { let #ident = #value; }
}

/// Generates the value expression of a map field (named or keyed) from the
/// child binding `child`, whose element value expression is `element`.
fn codec_decode_map_value(
    field: &ParsedField,
    child: &proc_macro2::TokenStream,
    element: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    if field.map_key_bytes == Some(true) {
        quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_keyed(kids, #name)?;
            let mut values = ::std::collections::BTreeMap::new();
            for kid in kids {
                let (key, content) = ::tree_space::tree::codec::split_keyed_field(kid, #name)?;
                values.insert(*key, #element);
            }
            values
        }}
    } else {
        quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_named(kids, #name)?;
            let mut values = ::std::collections::BTreeMap::new();
            for kid in kids {
                let (key, content) = ::tree_space::tree::codec::split_named_field(kid, #name)?;
                values.insert(key.to_owned(), #element);
            }
            values
        }}
    }
}

/// Generates a container-decode block over a block-map field.
fn codec_decode_block_map(
    field: &ParsedField,
    content: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    codec_decode_map(field, content)
}

/// Generates the decode block for an ephemeral field.
///
/// The field is materialized normally when the image carries it; a pruned
/// (absent) field falls back to the field type's `Default::default()` — the
/// compile-time contract that every ephemeral field type implements `Default`
/// (see `_dev/树与桶管道/01-目标与设计.md` §1.9.8 (a)4).
fn codec_decode_ephemeral_block(field: &ParsedField) -> proc_macro2::TokenStream {
    let ident = &field.ident;
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let present = codec_decode_present_expr(field, &quote! { child });
    quote! {
        let #ident = match ::tree_space::tree::codec::take_child(&mut map, #name)? {
            ::core::option::Option::Some(child) => #present,
            ::core::option::Option::None => ::core::default::Default::default(),
        };
    }
}

/// Produces the value expression of an ephemeral field whose image child is
/// present (`child` evaluates to the already-taken `&ImageField`).
fn codec_decode_present_expr(
    field: &ParsedField,
    child: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let leaf = &field.leaf_ty;
    let decode_tree = quote! { ::tree_space::tree::codec::DecodeTree::from_image_children };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Inline, MultiplicityKind::Single) => quote! {
            ::tree_space::tree::codec::expect_inline(#child, #name)?.clone()
        },
        (TargetKind::Inline, MultiplicityKind::Optional) => quote! {
            ::core::option::Option::Some(::tree_space::tree::codec::expect_inline(#child, #name)?.clone())
        },
        (TargetKind::Inline, MultiplicityKind::Sequence) => quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_positioned(kids, #name)?;
            kids.iter().map(|kid| ::tree_space::tree::codec::expect_inline(kid, #name).cloned()).collect::<::tree_space::Result<std::vec::Vec<_>>>()?
        }},
        (TargetKind::Inline, MultiplicityKind::Map) => codec_decode_map_value(
            field,
            child,
            &quote! {
                ::tree_space::tree::codec::expect_inline_of(content, #name)?.clone()
            },
        ),
        (TargetKind::Block, MultiplicityKind::Single) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {{
                let id = ::tree_space::tree::codec::expect_ref(#child, #name)?;
                ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?
            }}
        }
        (TargetKind::Block, MultiplicityKind::Optional) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {
                ::core::option::Option::Some({
                    let id = ::tree_space::tree::codec::expect_ref(#child, #name)?;
                    ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?
                })
            }
        }
        (TargetKind::Block, MultiplicityKind::Sequence) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {{
                let (kind, kids) = ::tree_space::tree::codec::expect_chunk_group(#child, #name)?;
                ::tree_space::tree::codec::check_chunk_kind(kind, &#kind, #name)?;
                let ids = ::tree_space::tree::codec::chunk_refs(kind, kids, #name)?;
                ids.into_iter()
                    .map(|id| ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, kind, bucket))
                    .collect::<::tree_space::Result<std::vec::Vec<_>>>()?
            }}
        }
        (TargetKind::Block, MultiplicityKind::Map) => {
            let kind = field.block_kind.clone().expect("block field kind");
            codec_decode_map_value(
                field,
                child,
                &quote! {{
                    let id = ::tree_space::tree::codec::expect_ref_of(content, #name)?;
                    ::tree_space::tree::codec::decode_block_leaf::<#leaf>(id, &#kind, bucket)?
                }},
            )
        }
        (TargetKind::Node, MultiplicityKind::Single) => quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            #decode_tree(kids, bucket)?
        }},
        (TargetKind::Node, MultiplicityKind::Optional) => quote! {
            ::core::option::Option::Some({
                let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
                #decode_tree(kids, bucket)?
            })
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_positioned(kids, #name)?;
            kids.iter()
                .map(|kid| #decode_tree(::tree_space::tree::codec::expect_node(kid, #name)?, bucket))
                .collect::<::tree_space::Result<std::vec::Vec<_>>>()?
        }},
        (TargetKind::Node, MultiplicityKind::Map) => codec_decode_map_value(
            field,
            child,
            &quote! {{
                let kids = ::tree_space::tree::codec::expect_node_of(content, #name)?;
                #decode_tree(kids, bucket)?
            }},
        ),
    }
}

/// Generates the L3 structure-check block for one field (数据验证系统 01 §6.3):
/// mirrors [`codec_decode_block`]'s structural validation (field set, leaf
/// shape, multiplicities, locator uniformity, chunk kinds) but every leaf
/// materialization is replaced by the kind/identity-only check against the
/// bucket — **no leaf payload is decoded**. The emitted statements feed
/// `DecodeTree::check_image_children`.
fn codec_check_block(field: &ParsedField) -> proc_macro2::TokenStream {
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let child = quote! { child };
    let present = codec_check_present(field, &child);
    let optional_take = quote! {
        if let ::core::option::Option::Some(child) =
            ::tree_space::tree::codec::take_child(&mut map, #name)?
        {
            #present
        }
    };
    if field.ephemeral {
        optional_take
    } else {
        match field.multiplicity {
            MultiplicityKind::Optional => optional_take,
            _ => quote! {
                let child = ::tree_space::tree::codec::require_child(&mut map, #name)?;
                #present
            },
        }
    }
}

/// Produces the L3 structure-check statements that validate an already-taken
/// image child (`child`) against this field's declared expectation — the
/// check-side mirror of [`codec_decode_present_expr`] with no leaf decoding.
fn codec_check_present(
    field: &ParsedField,
    child: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    let leaf = &field.leaf_ty;
    let check_tree = quote! {
        <#leaf as ::tree_space::tree::codec::DecodeTree>::check_image_children
    };
    match (field.target_kind, field.multiplicity) {
        (TargetKind::Inline, MultiplicityKind::Single | MultiplicityKind::Optional) => quote! {
            ::tree_space::tree::codec::expect_inline(#child, #name)?;
        },
        (TargetKind::Inline, MultiplicityKind::Sequence) => quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_positioned(kids, #name)?;
            for kid in kids {
                ::tree_space::tree::codec::expect_inline(kid, #name)?;
            }
        }},
        (TargetKind::Inline, MultiplicityKind::Map) => codec_check_map_value(
            field,
            child,
            &quote! {
                ::tree_space::tree::codec::expect_inline_of(content, #name)?;
            },
        ),
        (TargetKind::Block, MultiplicityKind::Single | MultiplicityKind::Optional) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {{
                let id = ::tree_space::tree::codec::expect_ref(#child, #name)?;
                ::tree_space::tree::codec::check_block_leaf_identity(id, &#kind, bucket)?;
            }}
        }
        (TargetKind::Block, MultiplicityKind::Sequence) => {
            let kind = field.block_kind.clone().expect("block field kind");
            quote! {{
                let (kind, kids) =
                    ::tree_space::tree::codec::expect_chunk_group(#child, #name)?;
                ::tree_space::tree::codec::check_chunk_kind(kind, &#kind, #name)?;
                let ids = ::tree_space::tree::codec::chunk_refs(kind, kids, #name)?;
                for id in ids {
                    ::tree_space::tree::codec::check_block_leaf_identity(id, kind, bucket)?;
                }
            }}
        }
        (TargetKind::Block, MultiplicityKind::Map) => {
            let kind = field.block_kind.clone().expect("block field kind");
            codec_check_map_value(
                field,
                child,
                &quote! {{
                    let id = ::tree_space::tree::codec::expect_ref_of(content, #name)?;
                    ::tree_space::tree::codec::check_block_leaf_identity(id, &#kind, bucket)?;
                }},
            )
        }
        (TargetKind::Node, MultiplicityKind::Single | MultiplicityKind::Optional) => quote! {
            #check_tree(::tree_space::tree::codec::expect_node(#child, #name)?, bucket)?;
        },
        (TargetKind::Node, MultiplicityKind::Sequence) => quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_positioned(kids, #name)?;
            for kid in kids {
                #check_tree(::tree_space::tree::codec::expect_node(kid, #name)?, bucket)?;
            }
        }},
        (TargetKind::Node, MultiplicityKind::Map) => codec_check_map_value(
            field,
            child,
            &quote! {{
                let kids = ::tree_space::tree::codec::expect_node_of(content, #name)?;
                #check_tree(kids, bucket)?;
            }},
        ),
    }
}

/// Produces the L3 structure-check statements over a map field (named or
/// keyed): a node at `child`, locator-category uniformity, and per-entry
/// element checks — the check-side mirror of [`codec_decode_map_value`] with
/// no leaf decoding.
fn codec_check_map_value(
    field: &ParsedField,
    child: &proc_macro2::TokenStream,
    element: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let name = syn::LitStr::new(&field.name, proc_macro2::Span::call_site());
    if field.map_key_bytes == Some(true) {
        quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_keyed(kids, #name)?;
            for kid in kids {
                let (_, content) =
                    ::tree_space::tree::codec::split_keyed_field(kid, #name)?;
                #element
            }
        }}
    } else {
        quote! {{
            let kids = ::tree_space::tree::codec::expect_node(#child, #name)?;
            ::tree_space::tree::codec::check_named(kids, #name)?;
            for kid in kids {
                let (_, content) =
                    ::tree_space::tree::codec::split_named_field(kid, #name)?;
                #element
            }
        }}
    }
}

fn parse_field(field: &Field) -> syn::Result<ParsedField> {
    let field_ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new_spanned(field, "unnamed field"))?;
    let name = field_ident.to_string();

    let (multiplicity, element_ty, map_key) = strip_containers(&field.ty)?;
    let (target_kind, _is_slot, target_tokens) = target_tokens(&field.ty, element_ty, map_key)?;
    let ephemeral = parse_tree_space_attrs(field)?;
    if ephemeral && target_kind != TargetKind::Node && multiplicity == MultiplicityKind::Single {
        return Err(syn::Error::new_spanned(
            field,
            "`#[tree_space(ephemeral)]` is only allowed on node fields (any cardinality) or \
             container fields (`Option`/`Vec`/`BTreeMap`); a Single leaf (Block/Inline) has no \
             natural empty form",
        ));
    }
    let leaf_ty = element_ty.unwrap_or(&field.ty);
    let leaf_ty = strip_leaf_slot(leaf_ty).0.to_owned();
    let block_kind = block_kind_tokens(&type_string(&leaf_ty));
    let map_key_bytes = map_key.map(|key| {
        let text = type_string(key);
        !(text == "String" || text == "::std::string::String" || text == "std::string::String")
    });
    Ok(ParsedField {
        name,
        ident: field_ident,
        multiplicity,
        target_kind,
        target_tokens,
        leaf_ty,
        block_kind,
        map_key_bytes,
        ephemeral,
    })
}

/// Parses the `#[tree_space(...)]` field attributes, returning whether the
/// `ephemeral` annotation is present. Unknown sub-attributes are rejected so a
/// typo cannot silently drop the annotation.
fn parse_tree_space_attrs(field: &Field) -> syn::Result<bool> {
    let mut ephemeral = false;
    for attr in &field.attrs {
        if !attr.path().is_ident("tree_space") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(EPHEMERAL_ATTR) {
                if ephemeral {
                    return Err(meta.error("duplicate `ephemeral` annotation"));
                }
                ephemeral = true;
                Ok(())
            } else {
                Err(meta.error("unsupported `#[tree_space(...)]` attribute"))
            }
        })?;
    }
    Ok(ephemeral)
}

/// Maps a leaf type name onto its static `BlockKind` literal, if it is a
/// tree-space block type.
fn block_kind_tokens(type_name: &str) -> Option<proc_macro2::TokenStream> {
    match type_name {
        "ArrowTable" | "tree_space::ArrowTable" | "tree_space::block::ArrowTable" => {
            Some(quote! { ::tree_space::BlockKind::Table })
        }
        "Sequence" | "tree_space::Sequence" | "tree_space::block::Sequence" => {
            Some(quote! { ::tree_space::BlockKind::Sequence })
        }
        "Kv" | "tree_space::Kv" | "tree_space::block::Kv" => {
            Some(quote! { ::tree_space::BlockKind::Kv })
        }
        "Blob" | "tree_space::Blob" | "tree_space::block::Blob" => {
            Some(quote! { ::tree_space::BlockKind::Blob })
        }
        _ => None,
    }
}

/// Strips `Option`/`Vec`/`BTreeMap` containers and returns the element type.
fn strip_containers(ty: &Type) -> syn::Result<(MultiplicityKind, Option<&Type>, Option<&Type>)> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return Ok((MultiplicityKind::Single, None, None));
    };
    let Some(last) = path.segments.last() else {
        return Ok((MultiplicityKind::Single, None, None));
    };
    let ident = last.ident.to_string();
    let generic_args = match &last.arguments {
        PathArguments::AngleBracketed(args) => args,
        _ => return Ok((MultiplicityKind::Single, None, None)),
    };
    let type_args = generic_args
        .args
        .iter()
        .filter_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect::<Vec<_>>();

    match ident.as_str() {
        "Option" => {
            if type_args.len() != 1 {
                return Err(syn::Error::new_spanned(
                    ty,
                    "Option expects one type argument",
                ));
            }
            Ok((MultiplicityKind::Optional, Some(type_args[0]), None))
        }
        "Vec" => {
            if type_args.len() != 1 {
                return Err(syn::Error::new_spanned(ty, "Vec expects one type argument"));
            }
            Ok((MultiplicityKind::Sequence, Some(type_args[0]), None))
        }
        "BTreeMap" => {
            if type_args.len() != 2 {
                return Err(syn::Error::new_spanned(
                    ty,
                    "BTreeMap expects two type arguments",
                ));
            }
            Ok((
                MultiplicityKind::Map,
                Some(type_args[1]),
                Some(type_args[0]),
            ))
        }
        _ => Ok((MultiplicityKind::Single, None, None)),
    }
}

/// Builds the runtime token stream and kind for a field's target.
///
/// Returns `(TargetKind, is_slot, tokens)`. The second value is retained only
/// for local parsing compatibility and is no longer part of the runtime model.
fn target_tokens(
    full_ty: &Type,
    element_ty: Option<&Type>,
    map_key: Option<&Type>,
) -> syn::Result<(TargetKind, bool, proc_macro2::TokenStream)> {
    if let Some(key) = map_key {
        let key_path = type_string(key);
        let is_allowed = key_path == "String"
            || key_path.ends_with("[u8; 16]")
            || key_path.ends_with("[u8;16]")
            || key_path == "::std::string::String"
            || key_path == "std::string::String";
        if !is_allowed {
            return Err(syn::Error::new_spanned(
                full_ty,
                "BTreeMap keys in a tree node must be a stable, computable key \
                 (String or [u8; 16]); arbitrary keys destabilize xpath addressing",
            ));
        }
    }

    let (leaf_ty, is_slot) = strip_leaf_slot(element_ty.unwrap_or(full_ty));
    let type_name = type_string(leaf_ty);

    let (kind, tokens) = match type_name.as_str() {
        "Value" | "tree_space::Value" | "tree_space::block::Value" => (
            TargetKind::Inline,
            quote! {
                ::tree_space::FieldTarget::Inline {
                    schema: &::tree_space::LeafSchemaMeta { name: "Value" },
                }
            },
        ),
        "ArrowTable" | "tree_space::block::ArrowTable" | "tree_space::ArrowTable" => (
            TargetKind::Block,
            quote! {
                ::tree_space::FieldTarget::Block {
                    kind: ::tree_space::BlockKind::Table,
                    schema: &::tree_space::LeafSchemaMeta { name: "Table" },
                }
            },
        ),
        "Sequence" | "tree_space::block::Sequence" | "tree_space::Sequence" => (
            TargetKind::Block,
            quote! {
                ::tree_space::FieldTarget::Block {
                    kind: ::tree_space::BlockKind::Sequence,
                    schema: &::tree_space::LeafSchemaMeta { name: "Sequence" },
                }
            },
        ),
        "Kv" | "tree_space::block::Kv" | "tree_space::Kv" => (
            TargetKind::Block,
            quote! {
                ::tree_space::FieldTarget::Block {
                    kind: ::tree_space::BlockKind::Kv,
                    schema: &::tree_space::LeafSchemaMeta { name: "Kv" },
                }
            },
        ),
        "Blob" | "tree_space::block::Blob" | "tree_space::Blob" => (
            TargetKind::Block,
            quote! {
                ::tree_space::FieldTarget::Block {
                    kind: ::tree_space::BlockKind::Blob,
                    schema: &::tree_space::LeafSchemaMeta { name: "Blob" },
                }
            },
        ),
        _ => (
            TargetKind::Node,
            quote! {
                ::tree_space::FieldTarget::Node(&<#leaf_ty as ::tree_space::TreeNodeMeta>::META)
            },
        ),
    };
    Ok((kind, is_slot, tokens))
}

/// Strips a generic `Slot<T>` wrapper when present.
fn strip_leaf_slot(ty: &Type) -> (&Type, bool) {
    let Type::Path(TypePath { path, .. }) = ty else {
        return (ty, false);
    };
    let Some(last) = path.segments.last() else {
        return (ty, false);
    };
    if last.ident.to_string() != "Slot" {
        return (ty, false);
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return (ty, false);
    };
    let inner = args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    });
    match inner {
        Some(inner) => (inner, true),
        None => (ty, false),
    }
}

/// Renders a type as a simple display string for name matching.
fn type_string(ty: &Type) -> String {
    match ty {
        Type::Path(TypePath { path, .. }) => path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Type::Array(array) => {
            let elem = type_string(&array.elem);
            let len = array.len.to_token_stream().to_string();
            format!("[{elem}; {len}]")
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_str;

    #[test]
    fn multiplicity_inference_from_containers() {
        let single: Type = parse_str("Scalar").unwrap();
        let (m, elem, key) = strip_containers(&single).unwrap();
        assert_eq!(m, MultiplicityKind::Single);
        assert!(elem.is_none());
        assert!(key.is_none());

        let optional: Type = parse_str("Option<Scalar>").unwrap();
        let (m, elem, key) = strip_containers(&optional).unwrap();
        assert_eq!(m, MultiplicityKind::Optional);
        assert!(elem.is_some());
        assert!(key.is_none());

        let sequence: Type = parse_str("Vec<Run>").unwrap();
        let (m, elem, key) = strip_containers(&sequence).unwrap();
        assert_eq!(m, MultiplicityKind::Sequence);
        assert!(elem.is_some());
        assert!(key.is_none());

        let map: Type = parse_str("BTreeMap<String, Run>").unwrap();
        let (m, elem, key) = strip_containers(&map).unwrap();
        assert_eq!(m, MultiplicityKind::Map);
        assert!(elem.is_some());
        assert!(key.is_some());
    }

    #[test]
    fn map_key_string_is_allowed() {
        let ty: Type = parse_str("BTreeMap<String, Scalar>").unwrap();
        let (_, elem, key) = strip_containers(&ty).unwrap();
        assert!(target_tokens(&ty, elem, key).is_ok());
    }

    #[test]
    fn map_key_bytes16_is_allowed() {
        let ty: Type = parse_str("BTreeMap<[u8; 16], Scalar>").unwrap();
        let (_, elem, key) = strip_containers(&ty).unwrap();
        assert!(target_tokens(&ty, elem, key).is_ok());
    }

    #[test]
    fn map_key_integer_is_rejected() {
        let ty: Type = parse_str("BTreeMap<u32, Scalar>").unwrap();
        let (_, elem, key) = strip_containers(&ty).unwrap();
        assert!(target_tokens(&ty, elem, key).is_err());
    }

    #[test]
    fn map_key_usize_is_rejected() {
        let ty: Type = parse_str("BTreeMap<usize, Scalar>").unwrap();
        let (_, elem, key) = strip_containers(&ty).unwrap();
        assert!(target_tokens(&ty, elem, key).is_err());
    }

    #[test]
    fn ephemeral_attr_is_parsed_on_container_and_node_fields() {
        let input: DeriveInput = parse_str(
            "struct S {\
                #[tree_space(ephemeral)] scratch: Vec<Sub>,\
                #[tree_space(ephemeral)] note: Option<Value>,\
                #[tree_space(ephemeral)] opts: BTreeMap<String, Sub>,\
                #[tree_space(ephemeral)] subtree: Sub,\
             }",
        )
        .unwrap();
        let fields = match input.data {
            Data::Struct(data) => data.fields,
            _ => unreachable!(),
        };
        for field in fields.iter() {
            let parsed = parse_field(field).unwrap();
            assert!(parsed.ephemeral, "ephemeral must parse for {}", parsed.name);
            if parsed.name == "subtree" {
                // A single node field is legal ephemeral (node fields accept
                // any cardinality).
                assert_eq!(parsed.target_kind, TargetKind::Node);
                assert_eq!(parsed.multiplicity, MultiplicityKind::Single);
            } else {
                assert_ne!(parsed.multiplicity, MultiplicityKind::Single);
            }
        }
    }

    #[test]
    fn ephemeral_attr_on_single_leaf_is_rejected() {
        let input: DeriveInput =
            parse_str("struct S { #[tree_space(ephemeral)] header: Blob }").unwrap();
        let Data::Struct(data) = input.data else {
            unreachable!()
        };
        let field = data.fields.iter().next().unwrap();
        let error = parse_field(field).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("ephemeral"),
            "compile-time rejection must mention ephemeral: {message}"
        );
    }

    #[test]
    fn ephemeral_attr_on_single_inline_is_rejected() {
        let input: DeriveInput =
            parse_str("struct S { #[tree_space(ephemeral)] tag: Value }").unwrap();
        let Data::Struct(data) = input.data else {
            unreachable!()
        };
        let field = data.fields.iter().next().unwrap();
        assert!(parse_field(field).is_err());
    }

    #[test]
    fn unknown_tree_space_attr_is_rejected() {
        let input: DeriveInput =
            parse_str("struct S { #[tree_space(volatile)] tag: Vec<Value> }").unwrap();
        let Data::Struct(data) = input.data else {
            unreachable!()
        };
        let field = data.fields.iter().next().unwrap();
        let error = parse_field(field).unwrap_err();
        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn ephemeral_decode_falls_back_to_default_on_missing_child() {
        let input: DeriveInput =
            parse_str("struct S { #[tree_space(ephemeral)] scratch: Vec<Sub> }").unwrap();
        let Data::Struct(data) = input.data else {
            unreachable!()
        };
        let field = data.fields.iter().next().unwrap();
        let parsed = parse_field(field).unwrap();
        assert!(parsed.ephemeral);
        let tokens = codec_decode_block(&parsed).to_string();
        assert!(
            tokens.contains("take_child")
                && tokens.contains("default")
                && tokens.contains("Default"),
            "ephemeral decode must take the child optionally and default on absence: {tokens}"
        );
        // A non-ephemeral vec field keeps the required (erroring) decode path.
        let plain: DeriveInput = parse_str("struct S { scratch: Vec<Sub> }").unwrap();
        let Data::Struct(plain_data) = plain.data else {
            unreachable!()
        };
        let plain_parsed = parse_field(plain_data.fields.iter().next().unwrap()).unwrap();
        let plain_tokens = codec_decode_block(&plain_parsed).to_string();
        assert!(
            plain_tokens.contains("require_child") && !plain_tokens.contains("default"),
            "non-ephemeral decode must keep the required child path: {plain_tokens}"
        );
    }
}
