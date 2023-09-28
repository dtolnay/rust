use crate::deriving;
use crate::deriving::generic::*;
use crate::deriving::path_std;

use rustc_ast::ptr::P;
use rustc_ast::{self as ast, MetaItem};
use rustc_data_structures::fx::FxHashSet;
use rustc_expand::base::{Annotatable, ExtCtxt};
use rustc_span::source_map::respan;
use rustc_span::symbol::{kw, sym, Ident};
use rustc_span::Span;
use thin_vec::ThinVec;

pub fn expand_deriving_eq(
    cx: &mut ExtCtxt<'_>,
    span: Span,
    mitem: &MetaItem,
    input_item: &Annotatable,
    push: &mut dyn FnMut(Annotatable),
    is_const: bool,
) {
    let span = cx.with_def_site_ctxt(span);

    let structural_trait_def = TraitDef {
        span,
        path: path_std!(marker::StructuralEq),
        skip_path_as_bound: true, // crucial!
        needs_copy_as_bound_if_packed: false,
        additional_bounds: Vec::new(),
        supports_unions: true,
        methods: Vec::new(),
        associated_types: Vec::new(),
        is_const: false,
    };
    structural_trait_def.expand(cx, mitem, input_item, push);

    let mut generated_eq_impl = None::<ast::Impl>;
    let trait_def = TraitDef {
        span,
        path: path_std!(cmp::Eq),
        skip_path_as_bound: false,
        needs_copy_as_bound_if_packed: true,
        additional_bounds: Vec::new(),
        supports_unions: true,
        methods: Vec::new(),
        associated_types: Vec::new(),
        is_const,
    };
    trait_def.expand_ext(
        cx,
        mitem,
        input_item,
        &mut |generated_annotatable| {
            assert!(
                generated_eq_impl.is_none(),
                "expected derive(Eq) to generate a single Eq impl",
            );
            if let Annotatable::Item(generated_item) = &generated_annotatable {
                if let ast::ItemKind::Impl(generated_item) = &generated_item.kind {
                    generated_eq_impl = Some((**generated_item).clone());
                    push(generated_annotatable);
                    return;
                }
            }
            unreachable!("expected derive(Eq) to generate a trait impl");
        },
        true,
    );

    if let Some(generated_eq_impl) = generated_eq_impl {
        let Annotatable::Item(input_item) = input_item else { unreachable!() };
        assert_fields_are_total_eq(cx, span, input_item, generated_eq_impl, push);
    }
}

// Ensure all fields are Eq by generating:
//
//     const _: () = {
//         trait AssertFieldsAreTotalEq {
//             const ASSERT_FIELDS_ARE_TOTAL_EQ: fn(&Self);
//         }
//         impl<$generics> AssertFieldsAreTotalEq for $SelfType {
//             const ASSERT_FIELDS_ARE_TOTAL_EQ: fn(&Self) = |_| {
//                 let _: ::core::cmp::AssertParamIsEq<$FieldType>;
//                 ...
//             };
//         }
//     };
//
fn assert_fields_are_total_eq(
    cx: &mut ExtCtxt<'_>,
    span: Span,
    input_item: &ast::Item,
    generated_eq_impl: ast::Impl,
    push: &mut dyn FnMut(Annotatable),
) {
    let mut let_stmts = ThinVec::new();
    let mut seen_type_names = FxHashSet::default();
    let mut process_variant = |variant: &ast::VariantData| {
        for field in variant.fields() {
            // This basic redundancy checking only prevents duplication of
            // assertions like `AssertParamIsEq<Foo>` where the type is a
            // simple name. That's enough to get a lot of cases, though.
            if let Some(name) = field.ty.kind.is_simple_path()
                && !seen_type_names.insert(name)
            {
                // Already produced an assertion for this type.
            } else {
                // let _: AssertParamIsEq<FieldTy>;
                super::assert_ty_bounds(
                    cx,
                    &mut let_stmts,
                    field.ty.clone(),
                    field.span,
                    &[sym::cmp, sym::AssertParamIsEq],
                );
            }
        }
    };

    match &input_item.kind {
        ast::ItemKind::Struct(variant, ..) | ast::ItemKind::Union(variant, ..) => {
            process_variant(variant);
        }
        ast::ItemKind::Enum(enum_def, ..) => {
            for variant in &enum_def.variants {
                process_variant(&variant.data);
            }
        }
        _ => unreachable!(),
    }

    // If there are no fields, no need to generated a check.
    if let_stmts.is_empty() {
        return;
    }

    // trait AssertFieldsAreTotalEq {
    //     const ASSERT_FIELDS_ARE_TOTAL_EQ: fn(&Self);
    // }
    let trait_name = Ident::new(sym::AssertFieldsAreTotalEq, span);
    let assoc_const_name = Ident::new(sym::ASSERT_FIELDS_ARE_TOTAL_EQ, span);
    let assoc_const_type = cx.ty(
        span,
        ast::TyKind::BareFn(P(ast::BareFnTy {
            unsafety: ast::Unsafe::No,
            ext: ast::Extern::None,
            generic_params: ThinVec::new(),
            decl: cx.fn_decl(
                ThinVec::from([ast::Param::from_self(
                    ast::AttrVec::new(),
                    respan(span, ast::SelfKind::Region(None, ast::Mutability::Not)),
                    Ident::new(kw::Empty, span),
                )]),
                ast::FnRetTy::Default(span),
            ),
            decl_span: span,
        })),
    );
    let trait_assert_fields_are_total_eq = cx.item(
        span,
        trait_name,
        ast::AttrVec::new(),
        ast::ItemKind::Trait(Box::new(ast::Trait {
            unsafety: ast::Unsafe::No,
            is_auto: ast::IsAuto::No,
            generics: ast::Generics::default(),
            bounds: ast::GenericBounds::new(),
            items: ThinVec::from([cx.item(
                span,
                assoc_const_name,
                ast::AttrVec::new(),
                ast::AssocItemKind::Const(Box::new(ast::ConstItem {
                    defaultness: ast::Defaultness::Final,
                    generics: ast::Generics::default(),
                    ty: assoc_const_type.clone(),
                    expr: None,
                })),
            )]),
        })),
    );

    // impl<$generics> AssertFieldsAreTotalEq for $SelfType {
    //     const ASSERT_FIELDS_ARE_TOTAL_EQ: fn(&Self) = |_| {
    //         let _: ::core::cmp::AssertParamIsEq<$FieldType>;
    //         ...
    //     };
    // }
    let impl_assert_fields_are_total_eq = cx.item(
        span,
        Ident::empty(),
        ast::AttrVec::new(),
        ast::ItemKind::Impl(Box::new(ast::Impl {
            unsafety: ast::Unsafe::No,
            polarity: ast::ImplPolarity::Positive,
            defaultness: ast::Defaultness::Final,
            constness: ast::Const::No,
            generics: generated_eq_impl.generics,
            of_trait: Some(cx.trait_ref(cx.path_ident(span, trait_name))),
            self_ty: generated_eq_impl.self_ty,
            items: ThinVec::from([cx.item(
                span,
                assoc_const_name,
                ast::AttrVec::new(),
                ast::AssocItemKind::Const(Box::new(ast::ConstItem {
                    defaultness: ast::Defaultness::Final,
                    generics: ast::Generics::default(),
                    ty: assoc_const_type,
                    expr: Some(cx.lambda_stmts_1(
                        span,
                        let_stmts,
                        Ident::new(kw::Underscore, span),
                    )),
                })),
            )]),
        })),
    );

    // const _: () = {
    //     $trait_assert_fields_are_total_eq
    //     $impl_assert_fields_are_total_eq
    // };
    push(Annotatable::Item(cx.item(
        span,
        Ident::new(kw::Underscore, span),
        deriving::lint_and_stability_attrs(&input_item.attrs).collect(),
        ast::ItemKind::Const(Box::new(ast::ConstItem {
            defaultness: ast::Defaultness::Final,
            generics: ast::Generics::default(),
            ty: cx.ty(span, ast::TyKind::Tup(ThinVec::new())),
            expr: Some(cx.expr_block(cx.block(
                span,
                ThinVec::from([
                    cx.stmt_item(span, trait_assert_fields_are_total_eq),
                    cx.stmt_item(span, impl_assert_fields_are_total_eq),
                ]),
            ))),
        })),
    )));
}
