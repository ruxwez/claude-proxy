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
    download_version(env!("CARGO_PKG_VERSION")).await
}
async fn download_version(version: &str) -> Result<PathBuf> {
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
        version
    );
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .https_only(true)
        .build()?;
    println!("Descargando servidor v{} para {os}/{arch}…", version);
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
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

fn binary_version(path: &std::path::Path) -> Result<semver::Version> {
    let output = std::process::Command::new(path).arg("--version").output()?;
    ensure!(
        output.status.success(),
        "No se pudo consultar la versión del servidor"
    );
    let text = String::from_utf8(output.stdout)?;
    semver::Version::parse(text.split_whitespace().last().context("Versión ausente")?)
        .context("Versión del servidor inválida")
}

pub async fn update() -> Result<String> {
    let directory = claude_proxy_service::local_directory()?.join("bin");
    let destination = directory.join(name());
    ensure!(
        destination.is_file(),
        "Instala primero el servidor local con claude-proxy service install"
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("server-update.lock"))?;
    lock.try_lock()
        .context("Otra actualización del servidor está en curso")?;
    let installed = binary_version(&destination)?;
    println!("Buscando la última versión estable del servidor local (actual: {installed})…");
    let release = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .https_only(true)
        .user_agent("claude-proxy-cli")
        .build()?
        .head("https://github.com/ruxwez/claude-proxy/releases/latest")
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        release.url().host_str() == Some("github.com"),
        "Origen de release inválido"
    );
    let tag = release
        .url()
        .path()
        .strip_prefix("/ruxwez/claude-proxy/releases/tag/")
        .context("Release sin tag")?;
    let latest = semver::Version::parse(tag.strip_prefix('v').context("Tag de release inválido")?)?;
    ensure!(latest.pre.is_empty(), "La release no es estable");
    if latest <= installed {
        return Ok(format!("Servidor local {installed}: ya está actualizado"));
    }
    // Verify and stage everything while the old server is still running.
    let downloaded = download_version(&latest.to_string()).await?;
    ensure!(
        binary_version(&downloaded)? == latest,
        "La versión del binario no coincide con la release"
    );
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    std::io::copy(&mut std::fs::File::open(downloaded)?, &mut staged)?;
    staged.as_file().sync_all()?;
    let staged = staged.into_temp_path();
    std::fs::set_permissions(&staged, std::fs::metadata(&destination)?.permissions())?;
    #[cfg(windows)]
    {
        // Keep the installed binary's private ACL, including LocalService RX.
        // The installer owns both files, so this does not require elevation.
        let quote =
            |path: &std::path::Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
        // PowerShell 7 hosts can export module paths incompatible with 5.1.
        let status = std::process::Command::new("powershell.exe").env_remove("PSModulePath").args([
            "-NoProfile", "-Command", &format!("$ErrorActionPreference='Stop'; Get-Acl -LiteralPath {} | Set-Acl -LiteralPath {}", quote(&destination), quote(&staged))
        ]).status()?;
        ensure!(
            status.success(),
            "No se pudieron conservar los permisos del servidor"
        );
    }
    let mut manage = |start| {
        claude_proxy_service::manage(&if start {
            claude_proxy_service::Action::Start
        } else {
            claude_proxy_service::Action::Stop
        })
    };
    replace_server(&destination, &staged, &mut manage, |updated| {
        wait_local_health(updated.then(|| latest.to_string()))
    })
    .await?;
    Ok(format!(
        "Servidor local actualizado: {installed} → {latest}"
    ))
}

async fn wait_local_health(expected: Option<String>) -> Result<()> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(1))
        .build()?;
    for _ in 0..30 {
        if let Ok(response) = http.get("http://127.0.0.1:10800/health").send().await
            && response.status().is_success()
            && let Ok(value) = response.json::<serde_json::Value>().await
            && value["status"] == "ok"
            && value["implementation"] == "rust"
            && expected
                .as_ref()
                .is_none_or(|version| value["version"].as_str() == Some(version.as_str()))
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("El servidor actualizado no respondió en el puerto local 10800")
}

async fn replace_server<F, H, R>(
    destination: &std::path::Path,
    staged: &std::path::Path,
    manage: &mut F,
    mut health: H,
) -> Result<()>
where
    F: FnMut(bool) -> Result<()>,
    H: FnMut(bool) -> R,
    R: std::future::Future<Output = Result<()>>,
{
    let backup =
        destination.with_file_name(format!("{}.previous-{}", name(), uuid::Uuid::new_v4()));
    manage(false)?;
    if let Err(error) = std::fs::rename(destination, &backup) {
        manage(true).context("No se sustituyó el binario y no se pudo reiniciar el servidor")?;
        return Err(error.into());
    }
    if let Err(error) = std::fs::rename(staged, destination) {
        std::fs::rename(&backup, destination)
            .with_context(|| format!("Recupera el binario anterior desde {}", backup.display()))?;
        manage(true).context("Binario anterior restaurado; no se pudo iniciar el servicio")?;
        return Err(error.into());
    }
    let result = match manage(true) {
        Ok(()) => health(true).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        manage(false).with_context(|| {
            format!(
                "No se pudo detener el servidor; copia anterior en {}",
                backup.display()
            )
        })?;
        std::fs::rename(destination, staged).with_context(|| {
            format!(
                "No se pudo retirar el binario fallido; copia anterior en {}",
                backup.display()
            )
        })?;
        std::fs::rename(&backup, destination)
            .with_context(|| format!("Recupera el binario anterior desde {}", backup.display()))?;
        manage(true).context("Binario anterior restaurado; no se pudo iniciar el servicio")?;
        health(false)
            .await
            .context("Binario anterior restaurado, pero no responde")?;
        anyhow::bail!("Actualización fallida; se restauró el servidor anterior: {error:#}");
    }
    std::fs::remove_file(backup)?;
    Ok(())
}

#[cfg(test)]
mod update_tests {
    use super::*;
    #[tokio::test]
    async fn denied_stop_leaves_both_binaries_intact() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(name());
        let staged = dir.path().join("new");
        std::fs::write(&destination, "old").unwrap();
        std::fs::write(&staged, "new").unwrap();
        let result = replace_server(
            &destination,
            &staged,
            &mut |_| Err(anyhow::anyhow!("permission denied")),
            |_| std::future::ready(Ok(())),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(staged).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    #[tokio::test]
    async fn failed_health_restores_previous_binary_and_restarts_service() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(name());
        let staged = dir.path().join("new");
        std::fs::write(&destination, "old").unwrap();
        std::fs::write(&staged, "new").unwrap();
        let mut actions = Vec::new();
        let mut checks = 0;
        let result = replace_server(
            &destination,
            &staged,
            &mut |start| {
                actions.push(start);
                Ok(())
            },
            |_| {
                checks += 1;
                std::future::ready(if checks == 1 {
                    Err(anyhow::anyhow!("failed health"))
                } else {
                    Ok(())
                })
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "old");
        assert_eq!(actions, [false, true, false, true]);
    }
    #[tokio::test]
    async fn verified_update_replaces_only_binary_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(name());
        let staged = dir.path().join("new");
        let data = dir.path().join("proxy.sqlite3");
        std::fs::write(&destination, "old").unwrap();
        std::fs::write(&staged, "new").unwrap();
        std::fs::write(&data, "settings and credentials").unwrap();
        replace_server(&destination, &staged, &mut |_| Ok(()), |_| {
            std::future::ready(Ok(()))
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(data).unwrap(),
            "settings and credentials"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
