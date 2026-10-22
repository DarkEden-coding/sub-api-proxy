use std::fmt::Write;
use std::path::Path;

fn collect(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(path)
        .expect("ui/sap/dist is required; run npm run build in ui/sap")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn main() {
    let root = Path::new("../../ui/sap/dist")
        .canonicalize()
        .expect("ui/sap/dist is required");
    println!("cargo:rerun-if-changed=../../ui/sap/dist");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut output =
        String::from("fn dashboard_asset(path: &str) -> Option<(&'static [u8], &'static str)> { match path {\n");
    for path in files {
        // The match key is a URL path, so it is joined with `/` on every host;
        // `display()` would give `\` on Windows. include_bytes! keeps the native path.
        let name: String = path
            .strip_prefix(&root)
            .unwrap()
            .components()
            .map(|part| format!("/{}", part.as_os_str().to_str().unwrap()))
            .collect();
        let mime = match path.extension().and_then(|s| s.to_str()).unwrap_or_default() {
            "html" => "text/html; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "svg" => "image/svg+xml",
            "woff2" => "font/woff2",
            "woff" => "font/woff",
            "ttf" => "font/ttf",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            "json" | "map" => "application/json",
            _ => "application/octet-stream",
        };
        writeln!(
            output,
            "{name:?} => Some((include_bytes!({:?}), {mime:?})),",
            path.to_str().unwrap()
        )
        .unwrap();
    }
    output.push_str("_ => None, } }\n");
    std::fs::write(
        Path::new(&std::env::var("OUT_DIR").unwrap()).join("dashboard.rs"),
        output,
    )
    .unwrap();
}
