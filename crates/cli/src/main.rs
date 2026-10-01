use std::path::Path;
use std::process::ExitCode;

use bumpalo::Bump;
use common::Ctx;
use common::diagnostic::{ANSII_CLEAR, ANSII_COLOR_RED, ANSII_UNDERLINED, DisplayDiagnostic};
use ide::{IdeDiagnostics, cargo};
use toml::MapTable;
use toml::parse::StringVal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    /// Validate arbitrary toml files.
    Validate,
    /// Check a `typst.toml` manifest.
    Check,
}

macro_rules! error {
    ($pat:expr) => {{
        eprint!("{ANSII_COLOR_RED}error: ");
        eprint!($pat);
        eprintln!("{ANSII_CLEAR}");
        return ExitCode::FAILURE;
    }};
}

macro_rules! input_error {
    ($pat:expr) => {{
        eprint!("{ANSII_COLOR_RED}argument error: ");
        eprint!($pat);
        eprintln!("{ANSII_CLEAR}");
        eprintln!();
        help_message();
        return ExitCode::FAILURE;
    }};
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    args.next();

    let Some(command_str) = args.next() else {
        input_error!("missing command");
    };
    let command = match command_str.as_str() {
        "validate" => Command::Validate,
        "check" => Command::Check,
        _ => input_error!("invalid command `{command_str}`"),
    };

    let Some(path) = args.next() else {
        input_error!("missing argument <file>");
    };
    if let Some(filename) = AsRef::<Path>::as_ref(&path).file_name() {
        if command == Command::Check && filename != "typst.toml" {
            input_error!(
                "file isn't named `typst.toml`, use the `validate` command for arbitrary toml files"
            );
        }
    } else {
        input_error!("<file> path is empty");
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => error!("error reading from file: {e}"),
    };

    let mut ctx = IdeDiagnostics::default();
    let bump = Bump::new();
    let tokens = toml::lex(&mut ctx, &bump, &path, &text);
    let ast = toml::parse(&mut ctx, &bump, tokens);
    let map = toml::map(&mut ctx, &bump, &ast);
    if command == Command::Check {
        check_typst_manifest(&mut ctx, map);
    }
    let _simple = toml::util::map_simple(&ast, map);

    ctx.sort_diagnostics();
    for error in ctx.errors.iter() {
        println!("{}", error.display(&ast.source));
    }
    for warning in ctx.warnings.iter() {
        println!("{}", warning.display(&ast.source));
    }
    for info in ctx.infos.iter() {
        println!("{}", info.display(&ast.source));
    }

    ExitCode::SUCCESS
}

fn check_typst_manifest(ctx: &mut IdeDiagnostics, map: &MapTable) {
    if let Some(package) = map.get("package").and_then(|e| e.node.as_table()) {
        if let Some(entrypoint) = package.get("entrypoint").and_then(|e| e.node.as_str()) {
            check_sanitized_path(ctx, "package.entrypoint", entrypoint);
        }
    }

    if let Some(template) = map.get("template").and_then(|e| e.node.as_table()) {
        if let Some(path) = template.get("path").and_then(|e| e.node.as_str()) {
            check_sanitized_path(ctx, "template.path", path);
        }
        if let Some(entrypoint) = template.get("entrypoint").and_then(|e| e.node.as_str()) {
            check_sanitized_path(ctx, "template.entrypoint", entrypoint);
        }
    }
}

fn check_sanitized_path(ctx: &mut IdeDiagnostics, toml_path: &str, path: &StringVal) {
    if path.text.starts_with(['/', '\\']) {
        ctx.error(cargo::Error::new(
            Box::new([]),
            toml_path.into(),
            path.text_span(),
            cargo::ErrorKind::Custom("is an absolute path".into()),
        ));
    }

    if path.text.contains("..") {
        ctx.error(cargo::Error::new(
            Box::new([]),
            toml_path.into(),
            path.text_span(),
            cargo::ErrorKind::Custom(" contains `..`".into()),
        ));
    }
}

fn help_message() {
    eprintln!("ctoml <command> <file>");
    eprintln!();
    eprintln!("commands:");
    eprintln!("  {ANSII_UNDERLINED}validate{ANSII_CLEAR}  to validate arbitrary toml files");
    eprintln!("  {ANSII_UNDERLINED}check{ANSII_CLEAR}     to check a `typst.toml` manifest");
}
