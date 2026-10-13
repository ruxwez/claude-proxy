use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn name() -> &'static str {
    if cfg!(windows) {
        "claude-proxy-server.exe"
    } else {
        "claude-proxy-server"
    }
}
pub async fn prepare_server() -> Result<PathBuf> {
    let sibling = std::env::current_exe()?
        .parent()
        .context("Falta directorio del CLI")?
        .join(name());
    let binary = if sibling.is_file() {
        sibling
    } else if let Some(binary) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name()))
            .find(|p| p.is_file())
    }) {
        binary
    } else {
        download().await?
    };
    // Stable managed copy: survives removal of a downloaded archive or cargo target.
    let directory = claude_proxy_service::local_directory()?.join("bin");
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(name());
    if binary != destination {
        // Never replace an executable already running (especially Windows). An
        // existing managed copy is reused; upgrades are an explicit operation.
        if !destination.exists() {
            let mut source = std::fs::File::open(&binary)?;
            let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
            std::io::copy(&mut source, &mut staged)?;
            staged.as_file().sync_all()?;
            match staged.persist_noclobber(&destination) {
                Ok(file) => drop(file),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.error.into()),
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(std::fs::canonicalize(destination)?)
}
async fn download() -> Result<PathBuf> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    ensure!(
        ["linux", "macos", "windows"].contains(&os) && ["x86_64", "aarch64"].contains(&arch),
        "Plataforma no soportada por las descargas; compila el servidor con Cargo"
    );
    let asset = format!(
        "claude-proxy-server-{os}-{arch}{}",
        if cfg!(windows) { ".exe" } else { "" }
    );
    let base = format!(
        "https://github.com/ruxwez/claude-proxy/releases/download/v{}",
        env!("CARGO_PKG_VERSION")
    );
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .https_only(true)
        .build()?;
    println!(
        "Descargando servidor v{} para {os}/{arch}…",
        env!("CARGO_PKG_VERSION")
    );
    let sum=http.get(format!("{base}/{asset}.sha256")).send().await?.error_for_status().context("No hay una versión publicada accesible; instala claude-proxy-server con Cargo o coloca el ejecutable junto al CLI")?.text().await?;
    let expected = sum.split_whitespace().next().context("Checksum ausente")?;
    ensure!(
        expected.len() == 64 && expected.chars().all(|c| c.is_ascii_hexdigit()),
        "Checksum inválido"
    );
    let mut response = http
        .get(format!("{base}/{asset}"))
        .send()
        .await?
        .error_for_status()?;
    let directory = claude_proxy_service::local_directory()?.join("downloads");
    std::fs::create_dir_all(&directory)?;
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    use std::io::Write;
    let mut hash = Sha256::new();
    let mut total = 0;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
        ensure!(total <= 128 * 1024 * 1024, "Ejecutable demasiado grande");
        hash.update(&chunk);
        file.write_all(&chunk)?;
    }
    ensure!(
        format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected),
        "El checksum del servidor no coincide"
    );
    file.as_file().sync_all()?;
    let path = directory.join(name());
    file.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}
pub fn install(binary: PathBuf) -> Result<()> {
    claude_proxy_service::install(&claude_proxy_service::Spec {
        binary,
        directory: claude_proxy_service::local_directory()?,
        host: "127.0.0.1".into(),
        port: 10800,
        admin_port: 10801,
        local: true,
        environment: Default::default(),
    })
}
