use std::{
    fs,
    path::{Path, PathBuf},
};

use syn::{
    Attribute, Item, LitStr, Meta, Token,
    parse::{ParseStream, Parser},
};

const PROTECTED_TABLES: [(&str, &[&str]); 51] = [
    ("content.columns", &["content/column/state.rs"]),
    (
        "content.changes",
        &["content/column/change.rs", "content/column/navigation.rs"],
    ),
    ("content.navigations", &["content/column/navigation.rs"]),
    ("content.appends", &["content/column/navigation.rs"]),
    ("content.completions", &["kernel/completion.rs"]),
    ("identity.revisions", &["identity/person/revision.rs"]),
    ("identity.persons", &["identity/person/state.rs"]),
    ("identity.origins", &["identity/person/record.rs"]),
    // The immutable union exposes fixed sources for these owners' projection pairings.
    // Base Person tables remain private to their declaring state/record modules.
    (
        "identity.changes",
        &[
            "identity/person/record.rs",
            "identity/account/record.rs",
            "access/holder.rs",
            "access/grant/record.rs",
        ],
    ),
    ("identity.corrections", &["identity/person/record.rs"]),
    ("identity.cancellations", &["identity/person/record.rs"]),
    ("center.profiles", &["center/affiliation/profile.rs"]),
    ("center.affiliations", &["center/affiliation/database.rs"]),
    ("center.completions", &["kernel/completion.rs"]),
    ("workforce.completions", &["kernel/completion.rs"]),
    ("workforce.numbers", &["workforce/number/state.rs"]),
    ("workforce.revisions", &["workforce/number/revision.rs"]),
    ("workforce.allocations", &["workforce/number/record.rs"]),
    ("workforce.corrections", &["workforce/number/record.rs"]),
    ("workforce.recoveries", &["workforce/number/record.rs"]),
    ("workforce.revocations", &["workforce/number/record.rs"]),
    ("workforce.transitions", &["workforce/number/record.rs"]),
    (
        "access.roles",
        &["access/role/state.rs", "access/role/database.rs"],
    ),
    (
        "access.changes",
        &["access/role/change.rs", "access/role/database.rs"],
    ),
    ("access.completions", &["kernel/completion.rs"]),
    ("access.holders", &["access/holder.rs"]),
    ("access.appointments", &["access/holder.rs"]),
    ("access.projections", &["access/holder.rs"]),
    (
        "access.grants",
        &[
            "access/grant/state.rs",
            "access/grant/record.rs",
            "access/grant/database.rs",
        ],
    ),
    (
        "access.assignments",
        &["access/grant/record.rs", "access/assignment/database.rs"],
    ),
    ("access.suspensions", &["access/grant/record.rs"]),
    ("access.resumptions", &["access/grant/record.rs"]),
    ("access.terminations", &["access/grant/record.rs"]),
    ("access.losses", &["access/grant/record.rs"]),
    (
        "access.invitations",
        &[
            "access/grant/invitation/state.rs",
            "access/grant/invitation/record.rs",
            "access/grant/invitation/database.rs",
        ],
    ),
    ("access.creations", &["access/grant/invitation/record.rs"]),
    ("access.adjustments", &["access/grant/invitation/record.rs"]),
    ("access.withdrawals", &["access/grant/invitation/record.rs"]),
    ("access.acceptances", &["access/grant/invitation/record.rs"]),
    (
        "access.invitation_transitions",
        &["access/grant/invitation/record.rs"],
    ),
    ("identity.accounts", &["identity/account/state.rs"]),
    ("identity.creations", &["identity/account/record.rs"]),
    ("identity.suspensions", &["identity/account/record.rs"]),
    ("identity.resumptions", &["identity/account/record.rs"]),
    ("identity.replacements", &["identity/account/record.rs"]),
    ("identity.logouts", &["identity/account/record.rs"]),
    ("identity.projections", &["identity/account/record.rs"]),
    ("identity.sessions", &["identity/session/state.rs"]),
    ("identity.issuances", &["identity/session/state.rs"]),
    ("identity.revocations", &["identity/session/state.rs"]),
    ("identity.completions", &["kernel/completion.rs"]),
];

fn test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

fn string_literals(input: ParseStream, strings: &mut Vec<String>) -> syn::Result<()> {
    // Token groups retain macro arguments that an expression-only walk would miss.
    while !input.is_empty() {
        if input.peek(Token![#]) && (input.peek2(syn::token::Bracket) || input.peek2(Token![!])) {
            let attributes = if input.peek2(Token![!]) {
                input.call(Attribute::parse_inner)?
            } else {
                input.call(Attribute::parse_outer)?
            };
            if test_only(&attributes) {
                if input.fork().parse::<Item>().is_ok() {
                    let _: Item = input.parse()?;
                } else if input.fork().parse::<syn::Arm>().is_ok() {
                    let _: syn::Arm = input.parse()?;
                } else {
                    let _: syn::Variant = input.parse()?;
                }
                continue;
            }
            for attribute in attributes {
                // Serialization labels describe public vocabulary, not SQL table access.
                if attribute.path().is_ident("doc") || attribute.path().is_ident("strum") {
                    continue;
                }
                match attribute.meta {
                    Meta::NameValue(value) => {
                        if let syn::Expr::Lit(expression) = value.value
                            && let syn::Lit::Str(value) = expression.lit
                        {
                            strings.push(value.value());
                        }
                    }
                    Meta::List(list) => {
                        let parser = |input: ParseStream| string_literals(input, strings);
                        parser.parse2(list.tokens)?;
                    }
                    Meta::Path(_) => {}
                }
            }
        } else if input.peek(LitStr) {
            strings.push(input.parse::<LitStr>()?.value());
        } else if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            string_literals(&content, strings)?;
        } else if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            string_literals(&content, strings)?;
        } else if input.peek(syn::token::Bracket) {
            let content;
            syn::bracketed!(content in input);
            string_literals(&content, strings)?;
        } else {
            input.step(|cursor| {
                cursor
                    .token_tree()
                    .map(|(_, next)| ((), next))
                    .ok_or_else(|| cursor.error("expected a Rust token"))
            })?;
        }
    }
    Ok(())
}

fn check_source(path: &Path, source: &str) -> Result<(), String> {
    let file = syn::parse_file(source)
        .map_err(|error| format!("{}: cannot parse Rust source: {error}", path.display()))?;
    if test_only(&file.attrs) {
        return Ok(());
    }
    let mut strings = Vec::new();
    let parser = |input: ParseStream| string_literals(input, &mut strings);
    parser
        .parse_str(source)
        .map_err(|error| format!("{}: cannot inspect Rust source: {error}", path.display()))?;
    for string in strings {
        let normalized = string
            .replace('"', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
            .replace(" .", ".")
            .replace(". ", ".");
        for (table, owners) in PROTECTED_TABLES {
            let names_table = normalized.match_indices(table).any(|(offset, _)| {
                let identifier = |character: char| {
                    character.is_alphanumeric() || character == '_' || character == '$'
                };
                !normalized[..offset]
                    .chars()
                    .next_back()
                    .is_some_and(identifier)
                    && !normalized[offset + table.len()..]
                        .chars()
                        .next()
                        .is_some_and(identifier)
            });
            if names_table
                && !owners.iter().any(|owner| path == Path::new(owner))
                && !path.starts_with("history")
            {
                return Err(format!(
                    "{}: protected table {table} belongs to {} or history",
                    path.display(),
                    owners.join(" or "),
                ));
            }
        }
    }
    Ok(())
}

fn rust_sources(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "{}: cannot read source directory: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!("{}: cannot read source entry: {error}", directory.display())
        })?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: cannot inspect source entry: {error}", path.display()))?;
        if kind.is_dir() {
            rust_sources(&path, paths)?;
        } else if kind.is_symlink() {
            return Err(format!(
                "{}: source symlinks need explicit coverage",
                path.display()
            ));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    Ok(())
}

fn external_test_modules(
    items: &[Item],
    directory: &Path,
    inherited: bool,
    paths: &mut Vec<PathBuf>,
) {
    for item in items {
        let Item::Mod(module) = item else {
            continue;
        };
        let directory = directory.join(module.ident.to_string());
        let test = inherited || test_only(&module.attrs);
        if let Some((_, items)) = &module.content {
            external_test_modules(items, &directory, test, paths);
        } else if test {
            let option = module.attrs.iter().find_map(|attribute| {
                if !attribute.path().is_ident("path") {
                    return None;
                }
                if let Meta::NameValue(value) = &attribute.meta
                    && let syn::Expr::Lit(expression) = &value.value
                    && let syn::Lit::Str(value) = &expression.lit
                {
                    return Some(value.value());
                }
                None
            });
            if let Some(path) = option {
                paths.push(directory.with_file_name(path));
            } else {
                paths.push(directory.with_extension("rs"));
                paths.push(directory);
            }
        }
    }
}

fn check_sources(root: &Path) -> Result<(), String> {
    let mut paths = Vec::new();
    rust_sources(root, &mut paths)?;
    paths.sort();
    if paths.is_empty() {
        return Err(format!("{}: no Rust sources found", root.display()));
    }
    let mut sources = Vec::new();
    let mut test_modules = Vec::new();
    for path in paths {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("{}: cannot read Rust source: {error}", path.display()))?;
        let file = syn::parse_file(&source)
            .map_err(|error| format!("{}: cannot parse Rust source: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("{}: source outside root: {error}", path.display()))?;
        let directory = if matches!(
            relative.file_name().and_then(|name| name.to_str()),
            Some("lib.rs" | "mod.rs")
        ) {
            relative.parent().unwrap_or(Path::new("")).to_owned()
        } else {
            relative.with_extension("")
        };
        external_test_modules(
            &file.items,
            &directory,
            test_only(&file.attrs),
            &mut test_modules,
        );
        sources.push((relative.to_owned(), source));
    }
    for (path, source) in sources {
        if !test_modules.iter().any(|test| path.starts_with(test)) {
            check_source(&path, &source)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{fs, path::Path};

    use crate::history::guard::{PROTECTED_TABLES, check_source, check_sources};

    #[test]
    fn protected_sql_stays_with_its_owner() {
        check_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
            .expect("protected SQL stays with its declaring module or history");
    }

    #[test]
    fn sql_guard_accepts_declaring_modules_and_history() {
        for (table, owners) in PROTECTED_TABLES {
            let source = format!("const TABLE: &str = {table:?};");
            for owner in owners {
                check_source(Path::new(owner), &source).expect("declaring module owns its table");
            }
            check_source(Path::new("history/startup.rs"), &source)
                .expect("history may inspect every protected table");
        }
    }

    #[test]
    fn capability_labels_do_not_grant_sql_access() {
        let source = r#"enum Capability {
            #[strum(serialize = "workforce.numbers")]
            WorkforceNumbers,
        }"#;
        check_source(Path::new("access/capability.rs"), source)
            .expect("serialized capability vocabulary is not SQL");
        let source = format!("{source} const SQL: &str = \"SELECT * FROM workforce.numbers\";");
        check_source(Path::new("access/capability.rs"), &source)
            .expect_err("capability definitions still cannot access number storage");
    }

    #[test]
    fn sql_guard_rejects_other_modules_and_wrong_owners() {
        for (table, _) in PROTECTED_TABLES {
            let source = format!("const SQL: &str = \"SELECT * FROM {table}\";");
            let error = check_source(Path::new("permission/manifest.rs"), &source)
                .expect_err("other modules cannot name protected tables");
            assert!(error.contains(table), "{error}");
        }
        let error = check_source(
            Path::new("center/affiliation/profile.rs"),
            r#"const SQL: &str = "SELECT * FROM center.affiliations";"#,
        )
        .expect_err("one declaring module does not own another table");
        assert!(
            error.contains("protected table center.affiliations"),
            "{error}"
        );
        let error = check_source(
            Path::new("history_extra.rs"),
            r#"const SQL: &str = "SELECT * FROM center.profiles";"#,
        )
        .expect_err("history permission uses a directory boundary");
        assert!(error.contains("protected table center.profiles"), "{error}");
    }

    #[test]
    fn sql_guard_rejects_case_whitespace_and_quoted_names() {
        for source in [
            r#"fn query() { let _ = "select * from CENTER.PROFILES"; }"#,
            r#"fn query() { let _ = "select * from center \n . \t profiles"; }"#,
            r##"fn query() { let _ = r#"select * from "center" . "profiles""#; }"##,
        ] {
            let error = check_source(Path::new("consumer.rs"), source)
                .expect_err("SQL spelling does not change table ownership");
            assert!(error.contains("protected table center.profiles"), "{error}");
        }
    }

    #[test]
    fn sql_guard_rejects_grant_sql_outside_its_declaring_modules() {
        for (path, table) in [
            ("workforce/number/database.rs", "access.grants"),
            ("access/role/database.rs", "access.assignments"),
            ("access/grant/lifecycle.rs", "access.suspensions"),
            ("access/grant/invitation/database.rs", "access.resumptions"),
            ("identity/cancellation.rs", "access.terminations"),
            ("access/grant/state.rs", "access.losses"),
        ] {
            let source = format!("fn query() {{ query!(\"SELECT * FROM {table}\"); }}");
            let error = check_source(Path::new(path), &source)
                .expect_err("Grant consumers call the declared state and record owners");
            assert!(
                error.contains(&format!("protected table {table}")),
                "{error}"
            );
        }
    }

    #[test]
    fn sql_guard_rejects_role_sql_outside_its_declaring_modules() {
        for (path, table, source) in [
            (
                "access/appointment/database.rs",
                "access.roles",
                r##"fn query() { execute!(r#"SELECT * FROM "ACCESS" . "ROLES""#); }"##,
            ),
            (
                "access/role/operation.rs",
                "access.changes",
                r#"mod nested { const SQL: &str = "select * from access \n . \t changes"; }"#,
            ),
            (
                "access/role/database.rs",
                "access.completions",
                r#"fn query() { execute!("SELECT * FROM access.completions"); }"#,
            ),
            (
                "access/role/state.rs",
                "access.changes",
                r#"const SQL: &str = "SELECT * FROM access.changes";"#,
            ),
            (
                "access/role/database.rs/extra.rs",
                "access.roles",
                r#"const SQL: &str = "SELECT * FROM access.roles";"#,
            ),
        ] {
            let error = check_source(Path::new(path), source)
                .expect_err("role ownership uses only the declared source paths");
            assert!(
                error.contains(&format!("protected table {table}")),
                "{error}"
            );
        }
    }

    #[test]
    fn sql_guard_rejects_identity_sql_outside_its_declaring_modules() {
        for (path, table) in [
            ("identity/current/database.rs", "identity.persons"),
            ("identity/account/record.rs", "identity.origins"),
            ("identity/correction/mod.rs", "identity.corrections"),
            ("identity/cancellation.rs", "identity.cancellations"),
            ("identity/person/state.rs", "identity.changes"),
            ("access/grant/mod.rs", "identity.changes"),
            ("identity/person/mod.rs", "identity.revisions"),
            ("identity/account/record.rs", "identity.revisions"),
            ("identity/correction/mod.rs", "identity.revisions"),
            ("registration/database.rs", "identity.revisions"),
            ("access/holder.rs", "identity.revisions"),
            ("identity/person/revision.rs/extra.rs", "identity.revisions"),
            ("identity/account/mod.rs", "identity.accounts"),
            ("identity/account/state.rs", "identity.creations"),
            ("identity/account/state.rs", "identity.suspensions"),
            ("identity/account/state.rs", "identity.resumptions"),
            ("identity/account/state.rs", "identity.replacements"),
            ("identity/account/state.rs", "identity.logouts"),
            ("identity/account/state.rs", "identity.projections"),
            ("identity/session/record.rs", "identity.sessions"),
            ("identity/session/mod.rs", "identity.issuances"),
            ("identity/revocation/command.rs", "identity.revocations"),
            (
                "identity/transfer/completion/database.rs",
                "identity.completions",
            ),
            ("identity/account/record.rs", "identity.completions"),
            ("kernel/completion.rs", "identity.replacements"),
            ("identity/account/state.rs/extra.rs", "identity.accounts"),
            ("identity/session/state.rs.extra", "identity.sessions"),
            ("access/holder.rs", "identity.projections"),
            ("identity/account/record.rs", "access.projections"),
        ] {
            let source = format!(r#"fn query() {{ execute!("SELECT * FROM {table}"); }}"#);
            let error = check_source(Path::new(path), &source)
                .expect_err("Identity ownership uses only the declared source paths");
            assert!(
                error.contains(&format!("protected table {table}")),
                "{error}"
            );
        }
    }

    #[test]
    fn sql_guard_rejects_number_sql_outside_its_declaring_modules() {
        for (path, table) in [
            ("workforce/number/command.rs", "workforce.numbers"),
            ("workforce/number/state.rs", "workforce.revisions"),
            ("workforce/number/revision.rs", "workforce.allocations"),
            ("workforce/number/query.rs", "workforce.transitions"),
            ("workforce/number/recovery.rs", "workforce.revocations"),
            ("workforce/number/mod.rs", "workforce.corrections"),
            ("workforce/number/state.rs", "workforce.recoveries"),
        ] {
            let source = format!(r#"fn query() {{ execute!("SELECT * FROM {table}"); }}"#);
            let error =
                check_source(Path::new(path), &source).expect_err("number SQL stays with its kind");
            assert!(
                error.contains(&format!("protected table {table}")),
                "{error}"
            );
        }
    }

    #[test]
    fn sql_guard_rejects_nested_modules_and_macro_strings() {
        for source in [
            r#"mod nested { fn query() { let _ = "center.profiles"; } }"#,
            r#"fn query() { execute!("SELECT * FROM center.profiles"); }"#,
            r##"macro_rules! query { () => { call!(r#"center.profiles"#) }; }"##,
            r#"#[query("SELECT * FROM center.profiles")] fn query() {}"#,
        ] {
            let error = check_source(Path::new("consumer.rs"), source)
                .expect_err("nested and macro literals retain their source owner");
            assert!(error.contains("protected table center.profiles"), "{error}");
        }
    }

    #[test]
    fn sql_guard_accepts_test_items_and_distinct_identifiers() {
        check_source(
            Path::new("consumer.rs"),
            r#"
            #[cfg(test)]
            mod tests { const SQL: &str = "center.profiles"; }
            mod nested {
                #[cfg(test)]
                fn example() { execute!("center.affiliations"); }
                const OTHER: &str = "center.profiles_archive mycenter.profiles";
            }
            /// The center.profiles table is protected.
            fn production() {}
            "#,
        )
        .expect("test items, documentation and different table names are outside the guard");
        check_source(
            Path::new("example.rs"),
            r#"#![cfg(test)] const SQL: &str = "center.profiles";"#,
        )
        .expect("test-only source is excluded");
    }

    #[test]
    fn sql_guard_accepts_test_match_arms_without_excluding_mixed_conditions() {
        let source = r#"
            //! Template examples for center.profiles.
            enum Family {
                Production,
                #[cfg(test)]
                Toy,
            }
            fn query(family: Family) -> &'static str {
                match family {
                    #[cfg(test)]
                    Family::Toy => "center.profiles",
                    _ => "",
                }
            }
        "#;
        check_source(Path::new("consumer.rs"), source)
            .expect("a test-only match arm is outside production SQL");
        for condition in [r#"any(test, feature = "example")"#, "not(test)"] {
            let source = source.replace("cfg(test)", &format!("cfg({condition})"));
            let error = check_source(Path::new("consumer.rs"), &source)
                .expect_err("a potentially production match arm remains covered");
            assert!(error.contains("protected table center.profiles"), "{error}");
        }
    }

    #[test]
    fn sql_guard_rejects_malformed_source() {
        let error = check_source(Path::new("broken.rs"), "fn broken( {")
            .expect_err("source parsing must fail closed");
        assert!(
            error.contains("broken.rs: cannot parse Rust source"),
            "{error}"
        );
    }

    #[test]
    fn sql_guard_excludes_external_test_modules() {
        let directory = tempfile::tempdir().expect("temporary source tree");
        fs::write(
            directory.path().join("lib.rs"),
            "#[cfg(test)] mod examples;",
        )
        .expect("write test module declaration");
        fs::write(
            directory.path().join("examples.rs"),
            r#"const SQL: &str = "center.profiles";"#,
        )
        .expect("write external test example");
        check_sources(directory.path()).expect("external test modules are excluded");
        fs::write(directory.path().join("lib.rs"), "mod examples;")
            .expect("make the module a production consumer");
        let error = check_sources(directory.path()).expect_err("production consumer is checked");
        assert!(error.contains("protected table center.profiles"), "{error}");

        let directory = tempfile::tempdir().expect("nested source tree");
        fs::create_dir(directory.path().join("examples")).expect("nested module directory");
        fs::write(
            directory.path().join("lib.rs"),
            "#[cfg(test)] mod examples { mod data; }",
        )
        .expect("write nested test module declaration");
        fs::write(
            directory.path().join("examples/data.rs"),
            r#"const SQL: &str = "center.profiles";"#,
        )
        .expect("write nested external test example");
        check_sources(directory.path()).expect("nested modules inherit the test exclusion");
    }

    #[test]
    fn sql_guard_rejects_missing_or_unreadable_sources() {
        let directory = tempfile::tempdir().expect("temporary source tree");
        check_sources(&directory.path().join("missing"))
            .expect_err("a missing root is a guard failure");
        check_sources(directory.path()).expect_err("an empty source tree is a guard failure");
        fs::write(directory.path().join("broken.rs"), [0xff]).expect("write invalid UTF-8 source");
        let error = check_sources(directory.path()).expect_err("source read must fail closed");
        assert!(error.contains("cannot read Rust source"), "{error}");
    }
}
