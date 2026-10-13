//! OS service lifecycle shared by the CLI and standalone server.
use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, process::Command as Process};

const NAME: &str = "claude-proxy";
#[derive(clap::Subcommand)]
pub enum Command {
    /// Registrar el servidor para arrancar con el sistema (requiere permisos del SO)
    Service {
        #[command(subcommand)]
        action: Action,
    },
}
#[derive(clap::Subcommand)]
pub enum Action {
    Install {
        #[arg(long)]
        config_dir: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 10800)]
        port: u16,
        #[arg(long, default_value_t = 10801)]
        admin_port: u16,
        /// JSON con variables de entorno del servidor; nunca se imprime
        #[arg(long)]
        env_file: Option<PathBuf>,
    },
    Start,
    Stop,
    Restart,
    Status,
}
#[derive(Clone)]
pub struct Spec {
    pub binary: PathBuf,
    pub directory: PathBuf,
    pub host: String,
    pub port: u16,
    pub admin_port: u16,
    pub local: bool,
    pub environment: std::collections::BTreeMap<String, String>,
}
pub fn local_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        return Ok(PathBuf::from(
            std::env::var_os("ProgramData").context("ProgramData no disponible")?,
        )
        .join("ClaudeProxy"));
    }
    #[cfg(not(windows))]
    {
        Ok(directories::ProjectDirs::from("", "", NAME)
            .context("Directorio de configuración no disponible")?
            .config_dir()
            .join("server"))
    }
}
impl Spec {
    fn arguments(&self) -> Vec<String> {
        let mut args = vec![
            "--config-dir".into(),
            self.directory.to_string_lossy().into(),
            "--host".into(),
            self.host.clone(),
            "--port".into(),
            self.port.to_string(),
            "--admin-port".into(),
            self.admin_port.to_string(),
        ];
        if self.local {
            args.extend([
                "--database".into(),
                self.directory
                    .join("proxy.sqlite3")
                    .to_string_lossy()
                    .into(),
            ]);
        }
        #[cfg(windows)]
        args.push("--windows-service".into());
        args
    }
}
pub fn execute(command: Command, local_binary: Option<PathBuf>) -> Result<()> {
    let Command::Service { action } = command;
    if let Action::Install {
        config_dir,
        host,
        port,
        admin_port,
        env_file,
    } = action
    {
        let local = local_binary.is_some();
        let binary = local_binary.unwrap_or(std::env::current_exe()?);
        let environment = match env_file {
            Some(file) => serde_json::from_slice(&std::fs::read(file)?)?,
            None => std::collections::BTreeMap::new(),
        };
        install(&Spec {
            binary,
            directory: config_dir.unwrap_or(local_directory()?),
            host,
            port,
            admin_port,
            local,
            environment,
        })?;
    } else {
        manage(&action)?;
    }
    Ok(())
}
fn checked(mut command: Process) -> Result<()> {
    // Environment/service configuration may contain credentials: never echo commands.
    ensure!(
        command
            .status()
            .context("No se pudo ejecutar el gestor de servicios")?
            .success(),
        "El gestor de servicios rechazó la operación"
    );
    Ok(())
}
#[cfg(unix)]
fn privileged(program: &str, args: &[&str]) -> Result<()> {
    let root = Process::new("id").arg("-u").output()?.stdout == b"0\n";
    let mut command = if root {
        Process::new(program)
    } else {
        let mut c = Process::new("sudo");
        c.arg(program);
        c
    };
    command.args(args);
    checked(command)
}
#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system/claude-proxy.service")
}
#[cfg(target_os = "macos")]
fn unit_path() -> PathBuf {
    PathBuf::from("/Library/LaunchDaemons/com.claude-proxy.server.plist")
}
#[cfg(unix)]
fn user() -> Result<String> {
    let output = Process::new("id").arg("-un").output()?;
    ensure!(
        output.status.success(),
        "No se pudo determinar el usuario del servicio"
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
#[cfg(target_os = "linux")]
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
#[cfg(target_os = "macos")]
fn quote(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
#[cfg(unix)]
pub fn definition(spec: &Spec, account: &str) -> Result<String> {
    let args = spec.arguments();
    ensure!(
        !spec.binary.to_string_lossy().contains(['\n', '\r'])
            && !spec.directory.to_string_lossy().contains(['\n', '\r']),
        "Ruta de servicio inválida"
    );
    for (key, value) in &spec.environment {
        ensure!(
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !value.contains(['\n', '\r', '\0']),
            "Variable de servicio inválida"
        );
    }
    #[cfg(target_os = "linux")]
    {
        // Secrets live in a root-readable EnvironmentFile, not the public unit.
        let env = if spec.local && spec.environment.is_empty() {
            String::new()
        } else {
            "EnvironmentFile=-/etc/claude-proxy.env\n".into()
        };
        Ok(format!(
            "[Unit]\nDescription=Claude Proxy\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser={account}\nWorkingDirectory={}\nExecStart={} {}\n{env}Restart=on-failure\nRestartSec=3\nTimeoutStopSec=20\nUMask=0077\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n",
            quote(&spec.directory.to_string_lossy()),
            quote(&spec.binary.to_string_lossy()),
            args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
        ))
    }
    #[cfg(target_os = "macos")]
    {
        let environment = spec
            .environment
            .iter()
            .map(|(k, v)| format!("<key>{}</key><string>{}</string>", quote(k), quote(v)))
            .collect::<String>();
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>com.claude-proxy.server</string><key>UserName</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string>{}</array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict>{environment}</dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>StandardOutPath</key><string>{}/service.log</string><key>StandardErrorPath</key><string>{}/service.log</string></dict></plist>",
            quote(account),
            quote(&spec.binary.to_string_lossy()),
            args.iter()
                .map(|s| format!("<string>{}</string>", quote(s)))
                .collect::<String>(),
            quote(&spec.directory.to_string_lossy()),
            quote(&spec.directory.to_string_lossy()),
            quote(&spec.directory.to_string_lossy())
        ))
    }
}
pub fn install(spec: &Spec) -> Result<()> {
    ensure!(
        spec.port > 0 && spec.admin_port > 0 && spec.port != spec.admin_port,
        "Puertos inválidos"
    );
    ensure!(
        spec.binary.is_file() && spec.binary.is_absolute() && spec.directory.is_absolute(),
        "El servicio requiere rutas absolutas y un ejecutable existente"
    );
    std::fs::create_dir_all(&spec.directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&spec.directory, std::fs::Permissions::from_mode(0o700))?;
        let mut file = tempfile::NamedTempFile::new()?;
        use std::io::Write;
        file.write_all(definition(spec, &user()?)?.as_bytes())?;
        let mode = if cfg!(target_os = "macos") {
            "600"
        } else {
            "644"
        };
        privileged(
            "install",
            &[
                "-m",
                mode,
                &file.path().to_string_lossy(),
                &unit_path().to_string_lossy(),
            ],
        )?;
        #[cfg(target_os = "linux")]
        if !spec.environment.is_empty() {
            let mut env = tempfile::NamedTempFile::new()?;
            for (key, value) in &spec.environment {
                // EnvironmentFile has no systemd ExecStart specifier expansion.
                writeln!(
                    env,
                    "{key}=\"{}\"",
                    value.replace('\\', "\\\\").replace('"', "\\\"")
                )?;
            }
            privileged(
                "install",
                &[
                    "-m",
                    "600",
                    &env.path().to_string_lossy(),
                    "/etc/claude-proxy.env",
                ],
            )?;
        }
        #[cfg(target_os = "linux")]
        {
            privileged("systemctl", &["daemon-reload"])?;
            privileged("systemctl", &["enable", NAME])?;
            manage(&Action::Restart)?;
        }
        #[cfg(target_os = "macos")]
        {
            let _ = privileged("launchctl", &["bootout", "system/com.claude-proxy.server"]);
            privileged("launchctl", &["enable", "system/com.claude-proxy.server"])?;
            privileged(
                "launchctl",
                &["bootstrap", "system", &unit_path().to_string_lossy()],
            )?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        windows::install(spec)
    }
}
pub fn manage(action: &Action) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let operation = match action {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Status => "status",
            _ => unreachable!(),
        };
        if matches!(action, Action::Status) {
            let mut c = Process::new("systemctl");
            c.args([operation, "--no-pager", NAME]);
            // systemctl status returns 3 for a stopped service, which is a valid status.
            let status = c.status()?;
            ensure!(
                status.success() || status.code() == Some(3),
                "Servicio no instalado o gestor no disponible"
            );
            Ok(())
        } else {
            privileged("systemctl", &[operation, NAME])
        }
    }
    #[cfg(target_os = "macos")]
    {
        let loaded = Process::new("launchctl")
            .args(["print", "system/com.claude-proxy.server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?
            .success();
        match action {
            Action::Start | Action::Restart => {
                privileged("launchctl", &["enable", "system/com.claude-proxy.server"])?;
                if loaded {
                    privileged(
                        "launchctl",
                        if matches!(action, Action::Restart) {
                            &["kickstart", "-k", "system/com.claude-proxy.server"]
                        } else {
                            &["kickstart", "system/com.claude-proxy.server"]
                        },
                    )
                } else {
                    privileged(
                        "launchctl",
                        &["bootstrap", "system", &unit_path().to_string_lossy()],
                    )
                }
            }
            Action::Stop => {
                privileged("launchctl", &["disable", "system/com.claude-proxy.server"])?;
                if loaded {
                    privileged("launchctl", &["bootout", "system/com.claude-proxy.server"])
                } else {
                    Ok(())
                }
            }
            Action::Status => {
                let mut c = Process::new("launchctl");
                c.args(["print", "system/com.claude-proxy.server"]);
                checked(c)
            }
            _ => unreachable!(),
        }
    }
    #[cfg(windows)]
    {
        windows::manage(action)
    }
}
#[cfg(windows)]
mod windows;
pub fn dispatch(run: fn() -> Result<()>) -> Result<()> {
    #[cfg(windows)]
    {
        windows::dispatch(run)
    }
    #[cfg(not(windows))]
    {
        let _ = run;
        anyhow::bail!("--windows-service solo se admite en Windows")
    }
}
pub async fn stop_requested() {
    #[cfg(windows)]
    loop {
        if windows::STOP.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    #[cfg(not(windows))]
    std::future::pending::<()>().await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn local_service_forces_sqlite_and_quotes_paths() {
        let spec = Spec {
            binary: PathBuf::from("/opt/Claude Proxy/server"),
            directory: PathBuf::from("/var/lib/Claude Proxy"),
            host: "127.0.0.1".into(),
            port: 10800,
            admin_port: 10801,
            local: true,
            environment: Default::default(),
        };
        let definition = definition(&spec, "testuser").unwrap();
        assert!(definition.contains("--database"));
        assert!(definition.contains("proxy.sqlite3"));
        assert!(definition.contains("127.0.0.1"));
        assert!(definition.contains("testuser"));
        #[cfg(target_os = "linux")]
        {
            assert!(definition.contains("WantedBy=multi-user.target"));
            assert!(definition.contains("Restart=on-failure"));
            assert!(!definition.contains("EnvironmentFile"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(definition.contains("RunAtLoad"));
            assert!(definition.contains("KeepAlive"));
        }
    }
    #[test]
    fn native_service_uses_same_environment_without_public_secrets() {
        let spec = Spec {
            binary: PathBuf::from("/opt/server"),
            directory: PathBuf::from("/var/lib/proxy"),
            host: "0.0.0.0".into(),
            port: 10800,
            admin_port: 10801,
            local: false,
            environment: std::collections::BTreeMap::from([(
                "AUTH_KEY".into(),
                "private-key".into(),
            )]),
        };
        let definition = definition(&spec, "proxy").unwrap();
        assert!(!definition.contains("--database"));
        #[cfg(target_os = "linux")]
        {
            assert!(!definition.contains("private-key"));
            assert!(definition.contains("/etc/claude-proxy.env"));
        }
    }
}
