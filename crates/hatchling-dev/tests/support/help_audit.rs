//! Resolve static environment keys; refuse unsupported dynamic readers.

use std::collections::{BTreeMap, BTreeSet};
use syn::visit::{self, Visit};
use syn::{
    Block, Expr, FnArg, ImplItemFn, ItemConst, ItemFn, ItemMod, Lit, Member, Pat, ReturnType, Stmt,
    Type, UseTree,
};

#[expect(
    clippy::redundant_pub_crate,
    reason = "integration-test support is visible only to its parent module"
)]
#[derive(Default, Debug)]
pub(super) struct Audit {
    pub keys: BTreeSet<String>,
    pub unsupported: Vec<String>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string() == "test"))
    })
}

fn string_return(output: &ReturnType) -> bool {
    matches!(output, ReturnType::Type(_, value) if matches!(&**value, Type::Reference(reference) if matches!(&*reference.elem, Type::Path(path) if path.path.is_ident("str"))))
}

fn imports(tree: &UseTree, prefix: &str, names: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => imports(&path.tree, &format!("{prefix}{}::", path.ident), names),
        UseTree::Group(group) => {
            for item in &group.items {
                imports(item, prefix, names);
            }
        }
        UseTree::Name(name) if matches!(name.ident.to_string().as_str(), "var" | "var_os") => {
            names.insert(name.ident.to_string());
        }
        UseTree::Rename(rename)
            if matches!(rename.ident.to_string().as_str(), "var" | "var_os") =>
        {
            names.insert(rename.rename.to_string());
        }
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) => {}
    }
}

#[derive(Default)]
struct Definitions<'ast> {
    constants: BTreeMap<String, Vec<&'ast Expr>>,
    methods: BTreeMap<String, Vec<&'ast Block>>,
    raw_reads: BTreeSet<String>,
    lookup_functions: BTreeMap<String, Vec<BTreeSet<usize>>>,
    impl_name: String,
    module_name: String,
}

impl<'ast> Visit<'ast> for Definitions<'ast> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !test_only(&item.attrs) {
            let old = std::mem::replace(&mut self.module_name, item.ident.to_string());
            visit::visit_item_mod(self, item);
            self.module_name = old;
        }
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let old = std::mem::replace(&mut self.impl_name, type_name(&item.self_ty));
        visit::visit_item_impl(self, item);
        self.impl_name = old;
    }
    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if !test_only(&item.attrs) {
            self.constants
                .entry(item.ident.to_string())
                .or_default()
                .push(&item.expr);
        }
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if !test_only(&item.attrs) {
            imports(&item.tree, "", &mut self.raw_reads);
        }
    }
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !test_only(&item.attrs) {
            let scope = if self.impl_name.is_empty() {
                &self.module_name
            } else {
                &self.impl_name
            };
            let target = qualified(scope, &item.sig.ident.to_string());
            self.lookup_functions
                .entry(target)
                .or_default()
                .push(lookup_positions(&item.sig));
            if string_return(&item.sig.output) && item.sig.inputs.is_empty() {
                self.methods
                    .entry(item.sig.ident.to_string())
                    .or_default()
                    .push(&item.block);
            }
            visit::visit_item_fn(self, item);
        }
    }
    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !test_only(&item.attrs) {
            let scope = if self.impl_name.is_empty() {
                &self.module_name
            } else {
                &self.impl_name
            };
            let target = qualified(scope, &item.sig.ident.to_string());
            self.lookup_functions
                .entry(target)
                .or_default()
                .push(lookup_positions(&item.sig));
            if string_return(&item.sig.output)
                && item
                    .sig
                    .inputs
                    .iter()
                    .all(|input| matches!(input, FnArg::Receiver(_)))
            {
                self.methods
                    .entry(item.sig.ident.to_string())
                    .or_default()
                    .push(&item.block);
            }
            visit::visit_impl_item_fn(self, item);
        }
    }
}

fn block_keys(
    block: &Block,
    defs: &Definitions<'_>,
    locals: &BTreeMap<String, Option<&Expr>>,
    depth: usize,
) -> Option<BTreeSet<String>> {
    if block.stmts.len() != 1 {
        return None;
    }
    match block.stmts.last()? {
        Stmt::Expr(value, _) => keys(value, defs, locals, depth + 1),
        Stmt::Local(_) | Stmt::Item(_) | Stmt::Macro(_) => None,
    }
}

fn keys(
    value: &Expr,
    defs: &Definitions<'_>,
    locals: &BTreeMap<String, Option<&Expr>>,
    depth: usize,
) -> Option<BTreeSet<String>> {
    if depth > 16 {
        return None;
    }
    match value {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(name) => Some(BTreeSet::from([name.value()])),
            Lit::ByteStr(_)
            | Lit::CStr(_)
            | Lit::Byte(_)
            | Lit::Char(_)
            | Lit::Int(_)
            | Lit::Float(_)
            | Lit::Bool(_)
            | Lit::Verbatim(_)
            | _ => None,
        },
        Expr::Field(field) if matches!(&field.member, Member::Named(name) if name == "name") => {
            descriptor_key(&field.base)
        }
        Expr::Path(path) => {
            let name = path.path.segments.last()?.ident.to_string();
            if let Some(value) = locals.get(&name) {
                return keys((*value)?, defs, locals, depth + 1);
            }
            let mut found = BTreeSet::new();
            for value in defs.constants.get(&name)? {
                found.extend(keys(value, defs, locals, depth + 1)?);
            }
            Some(found)
        }
        Expr::Reference(reference) => keys(&reference.expr, defs, locals, depth + 1),
        Expr::Paren(paren) => keys(&paren.expr, defs, locals, depth + 1),
        Expr::Group(group) => keys(&group.expr, defs, locals, depth + 1),
        Expr::Return(returned) => keys(returned.expr.as_deref()?, defs, locals, depth + 1),
        Expr::Block(block) => block_keys(&block.block, defs, locals, depth),
        Expr::Match(branches) => {
            let mut found = BTreeSet::new();
            for arm in &branches.arms {
                found.extend(keys(&arm.body, defs, locals, depth + 1)?);
            }
            Some(found)
        }
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "to_owned" | "to_string" | "as_str"
            ) =>
        {
            keys(&call.receiver, defs, locals, depth + 1)
        }
        Expr::MethodCall(call) if call.args.is_empty() => {
            function_keys(&call.method.to_string(), defs, locals, depth)
        }
        Expr::Call(call) if call.args.is_empty() => {
            let Expr::Path(path) = &*call.func else {
                return None;
            };
            function_keys(
                &path.path.segments.last()?.ident.to_string(),
                defs,
                locals,
                depth,
            )
        }
        Expr::Array(_)
        | Expr::Assign(_)
        | Expr::Async(_)
        | Expr::Await(_)
        | Expr::Binary(_)
        | Expr::Break(_)
        | Expr::Call(_)
        | Expr::Cast(_)
        | Expr::Closure(_)
        | Expr::Const(_)
        | Expr::Continue(_)
        | Expr::Field(_)
        | Expr::ForLoop(_)
        | Expr::If(_)
        | Expr::Index(_)
        | Expr::Infer(_)
        | Expr::Let(_)
        | Expr::Loop(_)
        | Expr::Macro(_)
        | Expr::MethodCall(_)
        | Expr::Range(_)
        | Expr::RawAddr(_)
        | Expr::Repeat(_)
        | Expr::Struct(_)
        | Expr::Try(_)
        | Expr::TryBlock(_)
        | Expr::Tuple(_)
        | Expr::Unary(_)
        | Expr::Unsafe(_)
        | Expr::Verbatim(_)
        | Expr::While(_)
        | Expr::Yield(_)
        | _ => None,
    }
}

fn descriptor_key(value: &Expr) -> Option<BTreeSet<String>> {
    let Expr::Path(path) = value else {
        return None;
    };
    let name = path.path.segments.last()?.ident.to_string();
    (name.starts_with("HATCHLING_") || name == "HOME").then(|| BTreeSet::from([name]))
}

fn function_keys(
    name: &str,
    defs: &Definitions<'_>,
    locals: &BTreeMap<String, Option<&Expr>>,
    depth: usize,
) -> Option<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for body in defs.methods.get(name)? {
        found.extend(block_keys(body, defs, locals, depth + 1)?);
    }
    Some(found)
}

fn lookup_closure(value: &Expr) -> Option<&syn::ExprClosure> {
    if let Expr::Reference(reference) = value {
        return lookup_closure(&reference.expr);
    }
    if let Expr::Paren(paren) = value {
        return lookup_closure(&paren.expr);
    }
    if let Expr::Group(group) = value {
        return lookup_closure(&group.expr);
    }
    if let Expr::Closure(closure) = value {
        return Some(closure);
    }
    None
}

fn bindings(pattern: &Pat) -> Vec<(String, bool)> {
    #[derive(Default)]
    struct Names(Vec<(String, bool)>);
    impl<'ast> Visit<'ast> for Names {
        fn visit_pat_ident(&mut self, name: &'ast syn::PatIdent) {
            self.0
                .push((name.ident.to_string(), name.mutability.is_none()));
            visit::visit_pat_ident(self, name);
        }
    }
    let mut names = Names::default();
    names.visit_pat(pattern);
    names.0
}

fn reader_path(value: &Expr) -> Option<&syn::ExprPath> {
    if let Expr::Path(value) = value {
        return Some(value);
    }
    if let Expr::Reference(value) = value {
        return reader_path(&value.expr);
    }
    if let Expr::Paren(value) = value {
        return reader_path(&value.expr);
    }
    if let Expr::Group(value) = value {
        return reader_path(&value.expr);
    }
    None
}

fn type_name(value: &Type) -> String {
    if let Type::Path(path) = value {
        return path
            .path
            .segments
            .last()
            .map_or_else(String::new, |segment| segment.ident.to_string());
    }
    String::new()
}

fn qualified(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_owned()
    } else {
        format!("{scope}::{name}")
    }
}

fn lookup_positions(sig: &syn::Signature) -> BTreeSet<usize> {
    sig.inputs
        .iter()
        .enumerate()
        .filter_map(|(position, input)| {
            let FnArg::Typed(input) = input else {
                return None;
            };
            lookup_type(&input.ty).then_some(position)
        })
        .collect()
}

fn lookup_parameters(sig: &syn::Signature) -> BTreeSet<String> {
    sig.inputs
        .iter()
        .filter_map(|input| {
            let FnArg::Typed(input) = input else {
                return None;
            };
            let Pat::Ident(name) = &*input.pat else {
                return None;
            };
            lookup_type(&input.ty).then(|| name.ident.to_string())
        })
        .collect()
}

fn lookup_type(value: &Type) -> bool {
    if let Type::Reference(reference) = value {
        return lookup_type(&reference.elem);
    }
    let Type::ImplTrait(value) = value else {
        return false;
    };
    value.bounds.iter().any(|bound| {
        let syn::TypeParamBound::Trait(bound) = bound else { return false; };
        let Some(segment) = bound.path.segments.last() else { return false; };
        let syn::PathArguments::Parenthesized(args) = &segment.arguments else { return false; };
        let ReturnType::Type(_, output) = &args.output else { return false; };
        let Type::Path(path) = &**output else { return false; };
        let Some(output) = path.path.segments.last() else { return false; };
        let syn::PathArguments::AngleBracketed(generic) = &output.arguments else { return false; };
        matches!(segment.ident.to_string().as_str(), "Fn" | "FnMut" | "FnOnce")
            && args.inputs.len() == 1
            && matches!(args.inputs.first(), Some(Type::Reference(reference)) if matches!(&*reference.elem, Type::Path(path) if path.path.is_ident("str")))
            && output.ident == "Option"
            && matches!(generic.args.first(), Some(syn::GenericArgument::Type(Type::Path(path))) if path.path.is_ident("String"))
    })
}

struct Reader<'defs, 'ast> {
    defs: &'defs Definitions<'ast>,
    audit: Audit,
    locals: BTreeMap<String, Option<&'ast Expr>>,
    delegated: BTreeSet<String>,
    function: String,
    lookups: BTreeSet<String>,
    known_callable: bool,
    delegate_closure: bool,
    impl_name: String,
    module_name: String,
}

impl Reader<'_, '_> {
    fn environment_reader(&self, name: &str) -> bool {
        matches!(name, "var" | "var_os")
            || self.defs.raw_reads.contains(name)
            || self.lookups.contains(name)
    }
    fn reader_expression(&self, value: &Expr) -> bool {
        reader_path(value)
            .and_then(|path| path.path.segments.last())
            .is_some_and(|segment| self.environment_reader(&segment.ident.to_string()))
    }
    fn lookup_target(&self, path: &syn::Path) -> String {
        let names: Vec<_> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if names.len() == 1 {
            return qualified(&self.module_name, &names.join("::"));
        }
        let mut names = names;
        if let Some(first) = names.first_mut() {
            if first == "Self" {
                first.clone_from(&self.impl_name);
            }
        }
        names.join("::")
    }
    fn invalidate_bindings(&mut self, pattern: &Pat) {
        for (name, _) in bindings(pattern) {
            self.delegated.remove(&name);
            self.locals.insert(name, None);
        }
    }
    fn invalidate_condition(&mut self, value: &Expr) {
        if let Expr::Let(value) = value {
            self.invalidate_bindings(&value.pat);
        }
        if let Expr::Binary(value) = value {
            self.invalidate_condition(&value.left);
            self.invalidate_condition(&value.right);
        }
        if let Expr::Paren(value) = value {
            self.invalidate_condition(&value.expr);
        }
        if let Expr::Group(value) = value {
            self.invalidate_condition(&value.expr);
        }
    }
    fn record(&mut self, value: &Expr, operation: &str) {
        if let Expr::Path(path) = value {
            if let Some(segment) = path.path.segments.last() {
                let name = segment.ident.to_string();
                if !self.locals.contains_key(&name)
                    && (self.delegated.contains(&name)
                        || (operation == "get" && self.function == "limit" && name == "key"))
                {
                    return;
                }
            }
        }
        match keys(value, self.defs, &self.locals, 0) {
            Some(names) if !names.is_empty() => self.audit.keys.extend(names),
            Some(_) | None => self.audit.unsupported.push(format!(
                "{}: unsupported dynamic key passed to {operation}",
                self.function
            )),
        }
    }
}

impl<'ast> Visit<'ast> for Reader<'_, 'ast> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !test_only(&item.attrs) {
            let old = std::mem::replace(&mut self.module_name, item.ident.to_string());
            visit::visit_item_mod(self, item);
            self.module_name = old;
        }
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let old = std::mem::replace(&mut self.impl_name, type_name(&item.self_ty));
        visit::visit_item_impl(self, item);
        self.impl_name = old;
    }
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !test_only(&item.attrs) {
            let old = std::mem::replace(&mut self.function, item.sig.ident.to_string());
            let locals = std::mem::take(&mut self.locals);
            let delegated = std::mem::take(&mut self.delegated);
            let lookups = std::mem::replace(&mut self.lookups, lookup_parameters(&item.sig));
            visit::visit_item_fn(self, item);
            self.locals = locals;
            self.lookups = lookups;
            self.delegated = delegated;
            self.function = old;
        }
    }
    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !test_only(&item.attrs) {
            let old = std::mem::replace(&mut self.function, item.sig.ident.to_string());
            let locals = std::mem::take(&mut self.locals);
            let delegated = std::mem::take(&mut self.delegated);
            let lookups = std::mem::replace(&mut self.lookups, lookup_parameters(&item.sig));
            visit::visit_impl_item_fn(self, item);
            self.locals = locals;
            self.lookups = lookups;
            self.delegated = delegated;
            self.function = old;
        }
    }
    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        let delegated = self.delegated.clone();
        let locals = self.locals.clone();
        let allow = self.delegate_closure;
        self.delegate_closure = false;
        for input in &closure.inputs {
            for (name, immutable) in bindings(input) {
                self.delegated.remove(&name);
                self.locals.insert(name.clone(), None);
                if allow && immutable {
                    self.locals.remove(&name);
                    self.delegated.insert(name);
                }
            }
        }
        visit::visit_expr_closure(self, closure);
        self.delegated = delegated;
        self.locals = locals;
        self.delegate_closure = allow;
    }
    fn visit_block(&mut self, block: &'ast Block) {
        let locals = self.locals.clone();
        visit::visit_block(self, block);
        self.locals = locals;
    }
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if !self.known_callable
            && path
                .path
                .segments
                .last()
                .is_some_and(|segment| self.environment_reader(&segment.ident.to_string()))
        {
            self.audit.unsupported.push(format!(
                "{}: unsupported environment-reader reference",
                self.function
            ));
        }
        visit::visit_expr_path(self, path);
    }
    fn visit_local(&mut self, local: &'ast syn::Local) {
        let known = self.known_callable;
        self.known_callable = false;
        // The initializer resolves before the new binding enters scope.
        visit::visit_local(self, local);
        self.known_callable = known;
        for (name, _) in bindings(&local.pat) {
            self.delegated.remove(&name);
            self.locals.insert(name, None);
        }
        if let (Pat::Ident(name), Some(init)) = (&local.pat, &local.init) {
            self.locals.insert(
                name.ident.to_string(),
                name.mutability.is_none().then_some(&*init.expr),
            );
        }
    }
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        let name = reader_path(&call.func)
            .and_then(|path| path.path.segments.last())
            .map(|segment| segment.ident.to_string());
        if let Some(name) = &name {
            if (name == "get" && !self.locals.contains_key(name)) || self.environment_reader(name) {
                if let Some(value) = call.args.first() {
                    self.record(value, name);
                }
            } else if name == "limit" {
                if let Some(value) = call.args.iter().nth(1) {
                    self.record(value, name);
                }
            }
        }
        let known = self.known_callable;
        self.known_callable = self.reader_expression(&call.func);
        self.visit_expr(&call.func);
        self.known_callable = known;
        let target =
            reader_path(&call.func).map_or_else(String::new, |path| self.lookup_target(&path.path));
        for (position, arg) in call.args.iter().enumerate() {
            let old = self.delegated.clone();
            let locals = self.locals.clone();
            let known = self.known_callable;
            let delegate = self.delegate_closure;
            let verified = self
                .defs
                .lookup_functions
                .get(&target)
                .is_some_and(|signatures| {
                    !signatures.is_empty()
                        && signatures
                            .iter()
                            .all(|positions| positions.contains(&position))
                });
            self.known_callable = verified && self.reader_expression(arg);
            self.delegate_closure = verified && lookup_closure(arg).is_some();
            self.visit_expr(arg);
            self.known_callable = known;
            self.delegate_closure = delegate;
            self.locals = locals;
            self.delegated = old;
        }
    }
    fn visit_expr_if(&mut self, branch: &'ast syn::ExprIf) {
        self.visit_expr(&branch.cond);
        let locals = self.locals.clone();
        let delegated = self.delegated.clone();
        self.invalidate_condition(&branch.cond);
        self.visit_block(&branch.then_branch);
        self.locals = locals;
        self.delegated = delegated;
        if let Some((_, value)) = &branch.else_branch {
            self.visit_expr(value);
        }
    }
    fn visit_expr_while(&mut self, loop_: &'ast syn::ExprWhile) {
        self.visit_expr(&loop_.cond);
        let locals = self.locals.clone();
        let delegated = self.delegated.clone();
        self.invalidate_condition(&loop_.cond);
        self.visit_block(&loop_.body);
        self.locals = locals;
        self.delegated = delegated;
    }
    fn visit_expr_for_loop(&mut self, loop_: &'ast syn::ExprForLoop) {
        self.visit_expr(&loop_.expr);
        let locals = self.locals.clone();
        let delegated = self.delegated.clone();
        self.invalidate_bindings(&loop_.pat);
        self.visit_block(&loop_.body);
        self.locals = locals;
        self.delegated = delegated;
    }
    fn visit_expr_match(&mut self, branches: &'ast syn::ExprMatch) {
        self.visit_expr(&branches.expr);
        let delegated = matches!(&*branches.expr, Expr::Path(path) if path.path.segments.last().is_some_and(|segment| self.delegated.contains(&segment.ident.to_string())));
        for arm in &branches.arms {
            let old = self.delegated.clone();
            let locals = self.locals.clone();
            for (name, immutable) in bindings(&arm.pat) {
                self.delegated.remove(&name);
                self.locals.insert(name.clone(), None);
                if delegated && immutable {
                    self.locals.remove(&name);
                    self.delegated.insert(name);
                }
            }
            self.visit_arm(arm);
            self.locals = locals;
            self.delegated = old;
        }
    }
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "integration-test support is visible only to its parent module"
)]
pub(super) fn audit(files: &[syn::File]) -> Audit {
    let mut defs = Definitions::default();
    for file in files {
        defs.visit_file(file);
    }
    let mut reader = Reader {
        defs: &defs,
        audit: Audit::default(),
        locals: BTreeMap::new(),
        delegated: BTreeSet::new(),
        function: String::new(),
        lookups: BTreeSet::new(),
        known_callable: false,
        delegate_closure: false,
        impl_name: String::new(),
        module_name: String::new(),
    };
    for file in files {
        reader.visit_file(file);
    }
    reader.audit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_resolves_constant_method_and_alias_keys() {
        let file = syn::parse_file(r#"
            use std::env::var as read;
            const KEY: &str = "HATCHLING_CONSTANT";
            impl Kind { const fn path_key(self) -> &'static str { match self { Self::A => "HATCHLING_METHOD_A", Self::B => "HATCHLING_METHOD_B" } } }
            fn reader(get: impl Fn(&str) -> Option<String>, kind: Kind) { get(KEY); get(kind.path_key()); read("HATCHLING_ALIAS"); let local = "HATCHLING_LOCAL".to_owned(); std::env::var(&local); }
        "#).unwrap();
        let result = audit(&[file]);
        assert!(result.unsupported.is_empty(), "{result:?}");
        assert_eq!(
            result.keys,
            BTreeSet::from(
                [
                    "HATCHLING_CONSTANT",
                    "HATCHLING_METHOD_A",
                    "HATCHLING_METHOD_B",
                    "HATCHLING_ALIAS",
                    "HATCHLING_LOCAL"
                ]
                .map(str::to_owned)
            )
        );
    }

    #[test]
    fn audit_refuses_unknown_dynamic_reads_instead_of_skipping_them() {
        for source in [
            r"fn f(name: &str) { std::env::var(name); }",
            r"use std::env::var as read; fn f(name: &str) { read(name); }",
            r#"fn f(get: impl Fn(&str) -> Option<String>) { get(format!("HATCHLING_{}", suffix).as_str()); }"#,
            r"impl Kind { fn path_key(self) -> &'static str { self.key } } fn f(get: impl Fn(&str) -> Option<String>, kind: Kind) { get(kind.path_key()); }",
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            assert!(
                !result.unsupported.is_empty(),
                "dynamic read skipped: {source}"
            );
        }
    }

    #[test]
    fn audit_tracks_alias_callbacks_imports_and_nested_scopes() {
        for source in [
            r#"fn f() { let read = std::env::var; read("HATCHLING_NEW"); }"#,
            r#"use std::env::var as get; fn f() { let read = get; read("HATCHLING_NEW"); }"#,
            r#"use std::env as e; use e::var as read; fn f() { read("HATCHLING_NEW"); }"#,
            r#"fn f() { let key = "HATCHLING_NEW"; { let key = "HATCHLING_KNOWN"; } std::env::var(key); }"#,
            r#"fn from_env(){ parse(|key| std::env::var(key).ok()); } fn parse(lookup: impl Fn(&str)->Option<String>){ lookup("HATCHLING_NEW"); }"#,
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            if source.contains("let read =") {
                assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
            } else {
                assert!(result.unsupported.is_empty(), "{source}: {result:?}");
                assert!(
                    result.keys.contains("HATCHLING_NEW"),
                    "{source}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn audit_refuses_mutable_keys_and_key_functions_with_early_returns() {
        for source in [
            r#"fn f() { let mut key = "HATCHLING_KNOWN"; key = "HATCHLING_NEW"; std::env::var(key); }"#,
            r#"fn key() -> &'static str { if condition { return "HATCHLING_NEW"; } "HATCHLING_KNOWN" } fn f(get: impl Fn(&str)->Option<String>) { get(key()); }"#,
            r"fn parse(lookup: Unknown) {} fn from_env() { parse(|key| std::env::var(key)); }",
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
        }
    }

    #[test]
    fn audit_handles_wrapped_readers_and_refuses_mutable_or_ambiguous_adapters() {
        for source in [
            r#"fn f() { (std::env::var)("HATCHLING_NEW"); }"#,
            r#"fn f() { let read = &std::env::var; read("HATCHLING_NEW"); }"#,
            r#"fn f() { let read = (std::env::var); read("HATCHLING_NEW"); }"#,
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            if source.contains("let read =") {
                assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
            } else {
                assert!(result.unsupported.is_empty(), "{source}: {result:?}");
                assert!(result.keys.contains("HATCHLING_NEW"));
            }
        }
        for source in [
            r#"fn from_env(){ parse(|key| std::env::var(key).ok()); } fn parse(lookup: impl Fn(&str)->Option<String>) { let (read, key) = (lookup, "HATCHLING_NEW"); read(key); }"#,
            r#"fn f() { let (read_text, read_os) = (std::env::var, std::env::var_os); read_text("HATCHLING_NEW"); read_os("HATCHLING_NEW_OS"); }"#,
            r#"fn f() { let mut read = std::env::var; read("HATCHLING_NEW"); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } mod other { fn parse(lookup: Unknown) {} fn from_env() { parse(|key| std::env::var(key)); } }"#,
            r#"fn parse(value: Value, lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env() { parse(|key| std::env::var(key), other_callback); }"#,
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
        }
    }

    #[test]
    fn audit_refuses_mutable_delegated_closure_and_match_bindings() {
        for source in [
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| match key { mut key => { key = "HATCHLING_NEW"; std::env::var(key).ok() } }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { values.map(|mut key| { key = "HATCHLING_NEW"; std::env::var(key) }); None }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { let (key,) = ("HATCHLING_NEW",); std::env::var(key).ok() }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { fn inner(key: &str) { std::env::var(key); } None }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|mut key| { key = "HATCHLING_NEW"; std::env::var(key).ok() }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| match key { mut other => { other = "HATCHLING_NEW"; std::env::var(other).ok() } }); }"#,
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
        }
    }

    #[test]
    fn audit_refuses_rebound_reader_aliases_and_conditional_key_shadowing() {
        for source in [
            r#"fn f() { let read = std::env::var; let read = read; read("HATCHLING_NEW"); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { if let Some(mut key) = optional_key { key = "HATCHLING_NEW"; std::env::var(key).ok() } else { None } }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { while let Some(mut key) = optional_key { key = "HATCHLING_NEW"; std::env::var(key); } None }); }"#,
            r#"fn parse(lookup: impl Fn(&str)->Option<String>) { lookup("HATCHLING_KNOWN"); } fn from_env(){ parse(|key| { for mut key in names { key = "HATCHLING_NEW"; std::env::var(key); } None }); }"#,
        ] {
            let result = audit(&[syn::parse_file(source).unwrap()]);
            assert!(!result.unsupported.is_empty(), "{source}: {result:?}");
        }
    }

    #[test]
    fn audit_distinguishes_local_json_getters_and_still_refuses_raw_reads() {
        let file = syn::parse_file(r#"fn f(value: Value) { let get = |key: &str| value.get(key); get("caught"); std::env::var("HATCHLING_REAL"); }"#).unwrap();
        let result = audit(&[file]);
        assert!(result.unsupported.is_empty(), "{result:?}");
        assert_eq!(result.keys, BTreeSet::from(["HATCHLING_REAL".to_owned()]));
        let file = syn::parse_file(
            r#"fn f() { let get = |key: &str| std::env::var(key); get("HATCHLING_DYNAMIC"); }"#,
        )
        .unwrap();
        assert!(!audit(&[file]).unsupported.is_empty());
    }

    #[test]
    fn audit_allows_lookup_adapters_but_not_arbitrary_dynamic_closures() {
        let adapter = syn::parse_file(r"fn from_env() { let key = load_master_key(); parse(|key| std::env::var(key).ok()); cgroup_parent(&|key| env_value(std::env::var(key))); } fn parse(get: impl Fn(&str) -> Option<String>) { get(env::HATCHLING_KNOWN.name); } fn cgroup_parent(get: &impl Fn(&str) -> Option<String>) { get(env::HATCHLING_GROUP.name); }").unwrap();
        let result = audit(&[adapter]);
        assert!(result.unsupported.is_empty(), "{result:?}");
        assert_eq!(
            result.keys,
            BTreeSet::from(["HATCHLING_KNOWN".to_owned(), "HATCHLING_GROUP".to_owned()])
        );
        let file = syn::parse_file(r"fn from_env() { parse(|key| std::env::var(key).ok()); } fn parse(get: impl Fn(&str) -> Option<String>) { get(env::HATCHLING_KNOWN.name); }").unwrap();
        let result = audit(&[file]);
        assert!(result.unsupported.is_empty(), "{result:?}");
        assert_eq!(result.keys, BTreeSet::from(["HATCHLING_KNOWN".to_owned()]));
        let result =
            audit(&[syn::parse_file(r"fn f() { values.map(|key| std::env::var(key)); }").unwrap()]);
        assert!(!result.unsupported.is_empty());
    }
}
