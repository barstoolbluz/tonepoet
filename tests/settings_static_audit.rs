use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprAssign, ExprCall, ExprField, ExprMethodCall, ExprParen, ExprReference,
    ExprStruct, ExprUnary, File, FnArg, GenericArgument, ItemFn, ItemMod, ItemType, Pat,
    PathArguments, ReturnType, Type, TypeReference, TypePath, TypeTuple, UseTree,
};

#[test]
fn production_code_does_not_construct_silent_pipeline_settings_defaults() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = BTreeSet::new();

    for path in rust_files_under(&root.join("src")) {
        audit_pipeline_settings_defaults(&path, &mut violations);
    }
    for path in rust_files_under(&root.join("tonepoet-pipeline").join("src")) {
        audit_pipeline_settings_defaults(&path, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "silent PipelineSettings default construction found:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn production_plan_request_literals_use_typed_pipeline_request_settings() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = BTreeSet::new();
    let mut literal_count = 0usize;

    for path in rust_files_under(&root.join("src")) {
        literal_count += audit_plan_request_literals(&path, &mut violations);
    }

    assert!(
        literal_count > 0,
        "no production PlanRequest struct literal was found under src/. The Chunk 2.1.1 invariant must be tested against the real per-track production constructor, not a test-only or newly added helper."
    );
    assert!(
        violations.is_empty(),
        "PlanRequest literals that do not carry settings from a typed PipelineRequest parameter found:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit_dir(root, &mut out);
    out.sort();
    out
}

fn visit_dir(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_dir(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn parse_rust_file(path: &Path) -> (String, File) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let parsed = syn::parse_file(&source)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
    (source, parsed)
}

fn audit_pipeline_settings_defaults(path: &Path, violations: &mut BTreeSet<String>) {
    let (source, parsed) = parse_rust_file(path);
    let names = collect_type_names(&parsed, "PipelineSettings");
    let mut audit = PipelineSettingsDefaultAudit {
        path,
        source: &source,
        pipeline_names: names,
        typed_pipeline_locals: HashSet::new(),
        pipeline_return_depth: 0,
        violations,
    };
    audit.visit_file(&parsed);
}

fn audit_plan_request_literals(path: &Path, violations: &mut BTreeSet<String>) -> usize {
    let (source, parsed) = parse_rust_file(path);
    let mut audit = PlanRequestLiteralAudit {
        path,
        source: &source,
        plan_request_names: collect_type_names(&parsed, "PlanRequest"),
        pipeline_request_names: collect_type_names(&parsed, "PipelineRequest"),
        function_contexts: Vec::new(),
        literal_count: 0,
        violations,
    };
    audit.visit_file(&parsed);
    audit.literal_count
}

fn collect_type_names(file: &File, canonical: &str) -> HashSet<String> {
    let mut names = HashSet::from([canonical.to_string()]);

    for item in &file.items {
        if let syn::Item::Use(item_use) = item {
            collect_use_aliases(&item_use.tree, canonical, &mut names);
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for item in &file.items {
            if let syn::Item::Type(item_type) = item {
                if type_contains_named(&item_type.ty, &names) && names.insert(item_type.ident.to_string()) {
                    changed = true;
                }
            }
        }
    }

    names
}

fn collect_use_aliases(tree: &UseTree, canonical: &str, names: &mut HashSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_aliases(&path.tree, canonical, names),
        UseTree::Name(name) if name.ident == canonical => {
            names.insert(name.ident.to_string());
        }
        UseTree::Rename(rename) if rename.ident == canonical => {
            names.insert(rename.rename.to_string());
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_aliases(item, canonical, names);
            }
        }
        UseTree::Glob(_) | UseTree::Name(_) | UseTree::Rename(_) => {}
    }
}

struct PipelineSettingsDefaultAudit<'a, 'b> {
    path: &'a Path,
    source: &'a str,
    pipeline_names: HashSet<String>,
    typed_pipeline_locals: HashSet<String>,
    pipeline_return_depth: usize,
    violations: &'b mut BTreeSet<String>,
}

impl PipelineSettingsDefaultAudit<'_, '_> {
    fn report(&mut self, span: proc_macro2::Span, detail: &str) {
        let line = span.start().line;
        if has_allowance_near_line(self.source, line) {
            return;
        }
        self.violations
            .insert(format!("{}:{line}: {detail}", self.path.display()));
    }
}

impl<'ast> Visit<'ast> for PipelineSettingsDefaultAudit<'_, '_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if has_cfg_test(&node.attrs) {
            return;
        }

        let returns_pipeline_settings = match &node.sig.output {
            ReturnType::Default => false,
            ReturnType::Type(_, ty) => type_contains_named(ty, &self.pipeline_names),
        };

        if returns_pipeline_settings {
            self.pipeline_return_depth += 1;
            syn::visit::visit_item_fn(self, node);
            self.pipeline_return_depth -= 1;
        } else {
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_item_type(&mut self, node: &'ast ItemType) {
        if type_contains_named(&node.ty, &self.pipeline_names) {
            self.pipeline_names.insert(node.ident.to_string());
        }
        syn::visit::visit_item_type(self, node);
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if let Pat::Type(pat_type) = &node.pat {
            if type_contains_named(&pat_type.ty, &self.pipeline_names) {
                if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                    self.typed_pipeline_locals.insert(pat_ident.ident.to_string());
                }
                if let Some(init) = &node.init {
                    if expr_is_default_call(&init.expr, &self.pipeline_names) {
                        self.report(
                            init.expr.span(),
                            "Default::default() assigned to PipelineSettings",
                        );
                    }
                }
            }
        }
        syn::visit::visit_local(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if expr_path_is_named_default_call(&node.func, &self.pipeline_names) {
            self.report(node.span(), "PipelineSettings::default()");
        } else if self.pipeline_return_depth > 0 && expr_path_is_trait_default_call(&node.func) {
            self.report(
                node.span(),
                "Default::default() in PipelineSettings-returning function",
            );
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        if expr_is_default_call(&node.right, &self.pipeline_names)
            && expr_is_typed_pipeline_local(&node.left, &self.typed_pipeline_locals)
        {
            self.report(
                node.right.span(),
                "Default::default() assigned to PipelineSettings local",
            );
        }
        syn::visit::visit_expr_assign(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast ExprStruct) {
        if path_last_segment_is_named(&node.path, &self.pipeline_names) {
            if let Some(rest) = &node.rest {
                if expr_is_default_call(rest, &self.pipeline_names) {
                    self.report(
                        rest.span(),
                        "PipelineSettings struct update uses Default::default()",
                    );
                }
            }
        }

        for field in &node.fields {
            if member_is_named(&field.member, "settings")
                && expr_path_is_trait_default_call_expr(&field.expr)
            {
                self.report(
                    field.expr.span(),
                    "Default::default() assigned to a settings field",
                );
            }
        }

        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let tokens = node.tokens.to_string();
        if tokens.contains("PipelineSettings") && tokens.contains("default") {
            self.report(node.span(), "macro mentions PipelineSettings default construction");
        }
        syn::visit::visit_macro(self, node);
    }
}

#[derive(Default)]
struct FunctionContext {
    pipeline_request_params: HashSet<String>,
}

struct PlanRequestLiteralAudit<'a, 'b> {
    path: &'a Path,
    source: &'a str,
    plan_request_names: HashSet<String>,
    pipeline_request_names: HashSet<String>,
    function_contexts: Vec<FunctionContext>,
    literal_count: usize,
    violations: &'b mut BTreeSet<String>,
}

impl PlanRequestLiteralAudit<'_, '_> {
    fn report(&mut self, span: proc_macro2::Span, detail: &str) {
        let line = span.start().line;
        if has_allowance_near_line(self.source, line) {
            return;
        }
        self.violations
            .insert(format!("{}:{line}: {detail}", self.path.display()));
    }

    fn current_pipeline_request_params(&self) -> HashSet<String> {
        self.function_contexts
            .last()
            .map(|ctx| ctx.pipeline_request_params.clone())
            .unwrap_or_default()
    }
}

impl<'ast> Visit<'ast> for PlanRequestLiteralAudit<'_, '_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if has_cfg_test(&node.attrs) {
            return;
        }

        let mut ctx = FunctionContext::default();
        for input in &node.sig.inputs {
            if let FnArg::Typed(pat_type) = input {
                if type_contains_named(&pat_type.ty, &self.pipeline_request_names) {
                    if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                        ctx.pipeline_request_params.insert(pat_ident.ident.to_string());
                    }
                }
            }
        }

        self.function_contexts.push(ctx);
        syn::visit::visit_item_fn(self, node);
        self.function_contexts.pop();
    }

    fn visit_expr_struct(&mut self, node: &'ast ExprStruct) {
        if path_last_segment_is_named(&node.path, &self.plan_request_names) {
            self.literal_count += 1;
            let params = self.current_pipeline_request_params();
            if !plan_request_literal_preserves_settings(node, &params) {
                self.report(
                    node.span(),
                    "PlanRequest literal must set settings directly from a typed PipelineRequest parameter, for example settings: request.settings.clone()",
                );
            }
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let tokens = node.tokens.to_string();
        if tokens.contains("PlanRequest") {
            self.report(node.span(), "macro mentions PlanRequest construction");
        }
        syn::visit::visit_macro(self, node);
    }
}

fn plan_request_literal_preserves_settings(
    node: &ExprStruct,
    pipeline_request_params: &HashSet<String>,
) -> bool {
    node.fields.iter().any(|field| {
        member_is_named(&field.member, "settings")
            && expr_carries_pipeline_request_settings(&field.expr, pipeline_request_params)
    })
}

fn expr_carries_pipeline_request_settings(expr: &Expr, pipeline_request_params: &HashSet<String>) -> bool {
    match strip_wrappers(expr) {
        Expr::MethodCall(ExprMethodCall { receiver, method, .. })
            if method == "clone" || method == "to_owned" =>
        {
            expr_carries_pipeline_request_settings(receiver, pipeline_request_params)
        }
        Expr::Field(field) => expr_field_reads_pipeline_request_settings(field, pipeline_request_params),
        _ => false,
    }
}

fn expr_field_reads_pipeline_request_settings(
    field: &ExprField,
    pipeline_request_params: &HashSet<String>,
) -> bool {
    member_is_named(&field.member, "settings")
        && expr_is_pipeline_request_param_base(&field.base, pipeline_request_params)
}

fn expr_is_pipeline_request_param_base(expr: &Expr, pipeline_request_params: &HashSet<String>) -> bool {
    match strip_wrappers(expr) {
        Expr::Path(path) if path.path.segments.len() == 1 => path
            .path
            .segments
            .first()
            .map(|segment| pipeline_request_params.contains(&segment.ident.to_string()))
            .unwrap_or(false),
        Expr::Unary(ExprUnary { expr, .. }) => expr_is_pipeline_request_param_base(expr, pipeline_request_params),
        _ => false,
    }
}

fn strip_wrappers(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(ExprParen { expr, .. })
        | Expr::Reference(ExprReference { expr, .. })
        | Expr::Group(syn::ExprGroup { expr, .. }) => strip_wrappers(expr),
        _ => expr,
    }
}

fn member_is_named(member: &syn::Member, expected: &str) -> bool {
    matches!(member, syn::Member::Named(ident) if ident == expected)
}

fn expr_is_typed_pipeline_local(expr: &Expr, typed_pipeline_locals: &HashSet<String>) -> bool {
    match expr {
        Expr::Path(path) if path.path.segments.len() == 1 => path
            .path
            .segments
            .first()
            .map(|segment| typed_pipeline_locals.contains(&segment.ident.to_string()))
            .unwrap_or(false),
        _ => false,
    }
}

fn expr_is_default_call(expr: &Expr, pipeline_names: &HashSet<String>) -> bool {
    match expr {
        Expr::Call(call) => {
            expr_path_is_named_default_call(&call.func, pipeline_names)
                || expr_path_is_trait_default_call(&call.func)
        }
        _ => false,
    }
}

fn expr_path_is_trait_default_call_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => expr_path_is_trait_default_call(&call.func),
        _ => false,
    }
}

fn expr_path_is_named_default_call(expr: &Expr, type_names: &HashSet<String>) -> bool {
    let Expr::Path(path) = expr else {
        return false;
    };
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    if segments.len() < 2 || segments.last().map(String::as_str) != Some("default") {
        return false;
    }
    segments
        .get(segments.len() - 2)
        .map(|name| type_names.contains(name))
        .unwrap_or(false)
}

fn expr_path_is_trait_default_call(expr: &Expr) -> bool {
    let Expr::Path(path) = expr else {
        return false;
    };
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();

    (segments.len() == 2 && segments[0] == "Default" && segments[1] == "default")
        || (segments.len() == 4
            && segments[0] == "std"
            && segments[1] == "default"
            && segments[2] == "Default"
            && segments[3] == "default")
}

fn type_contains_named(ty: &Type, names: &HashSet<String>) -> bool {
    match ty {
        Type::Path(type_path) => type_path_contains_named(type_path, names),
        Type::Reference(TypeReference { elem, .. }) => type_contains_named(elem, names),
        Type::Tuple(TypeTuple { elems, .. }) => elems.iter().any(|elem| type_contains_named(elem, names)),
        _ => false,
    }
}

fn type_path_contains_named(type_path: &TypePath, names: &HashSet<String>) -> bool {
    if path_last_segment_is_named(&type_path.path, names) {
        return true;
    }

    type_path.path.segments.iter().any(|segment| match &segment.arguments {
        PathArguments::AngleBracketed(args) => args.args.iter().any(|arg| match arg {
            GenericArgument::Type(ty) => type_contains_named(ty, names),
            _ => false,
        }),
        PathArguments::Parenthesized(args) => {
            args.inputs.iter().any(|ty| type_contains_named(ty, names))
                || match &args.output {
                    ReturnType::Default => false,
                    ReturnType::Type(_, ty) => type_contains_named(ty, names),
                }
        }
        PathArguments::None => false,
    })
}

fn path_last_segment_is_named(path: &syn::Path, names: &HashSet<String>) -> bool {
    path.segments
        .last()
        .map(|segment| names.contains(&segment.ident.to_string()))
        .unwrap_or(false)
}

fn has_allowance_near_line(source: &str, line: usize) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return false;
    }
    let start = line.saturating_sub(4).max(1);
    let end = line.min(lines.len());
    lines[start - 1..end]
        .iter()
        .any(|text| text.contains("settings-sentinel-allow"))
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        match &attr.meta {
            syn::Meta::List(list) => list.tokens.to_string().contains("test"),
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::NameValue(_) => false,
        }
    })
}
