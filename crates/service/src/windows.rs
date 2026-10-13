use super::*;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

pub static STOP: AtomicBool = AtomicBool::new(false);
static RUN: OnceLock<fn() -> Result<()>> = OnceLock::new();
define_windows_service!(ffi_main, service_main);
fn service_main(_: Vec<std::ffi::OsString>) {
    let handle = match service_control_handler::register(NAME, |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            STOP.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) {
        Ok(h) => h,
        Err(_) => return,
    };
    let status = |state, exit| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(exit),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    };
    let _ = handle.set_service_status(status(ServiceState::Running, 0));
    let result = RUN.get().expect("service runner")();
    let _ = handle.set_service_status(status(
        ServiceState::Stopped,
        if result.is_ok() { 0 } else { 1 },
    ));
}
pub fn dispatch(run: fn() -> Result<()>) -> Result<()> {
    // Set before the dispatcher or Tokio creates threads. Windows environment
    // APIs are synchronized, and this is the only environment mutation.
    let args: Vec<_> = std::env::args().collect();
    if let Some(index) = args.iter().position(|a| a == "--config-dir") {
        let path = PathBuf::from(args.get(index + 1).context("Falta config-dir")?)
            .join("service-env.json");
        if path.exists() {
            let values: std::collections::BTreeMap<String, String> =
                serde_json::from_slice(&std::fs::read(path)?)?;
            for (key, value) in values {
                unsafe {
                    std::env::set_var(key, value);
                }
            }
        }
    }
    let _ = RUN.set(run);
    service_dispatcher::start(NAME, ffi_main)?;
    Ok(())
}
fn ps(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
fn elevate(script: &str) -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::Builder::new().suffix(".ps1").tempfile()?;
    file.write_all(format!("$ErrorActionPreference='Stop'\ntry {{\n{script}\n}} catch {{ [Console]::Error.WriteLine('El gestor de servicios rechazó la operación'); exit 1 }}\n").as_bytes())?;
    let path = ps(&file.path().to_string_lossy());
    let wrapper = format!(
        r#"$p=[Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent(); if($p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {{ & {path} }} else {{ $proc=Start-Process powershell.exe -Verb RunAs -Wait -PassThru -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',('"'+{path}+'"')); exit $proc.ExitCode }}"#
    );
    let mut command = Process::new("powershell.exe");
    command.args(["-NoProfile", "-Command", &wrapper]);
    checked(command)
}
pub fn install(spec: &Spec) -> Result<()> {
    // The server and its data are owned by the installer; LocalService only
    // receives read/execute on the binary and modify on the private data folder.
    let identity = Process::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()?;
    ensure!(
        identity.status.success(),
        "No se pudo determinar el usuario instalador"
    );
    let identity = String::from_utf8(identity.stdout)?;
    let sid = identity
        .split(',')
        .nth(1)
        .context("SID no disponible")?
        .trim()
        .trim_matches('"');
    ensure!(
        sid.starts_with("S-1-")
            && sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-'),
        "SID inválido"
    );
    let caller = ps(&format!("*{sid}:(OI)(CI)F"));
    let owner = ps(&format!("*{sid}:F"));
    let bin = spec.binary.to_string_lossy();
    let mut args = vec![format!("\"{bin}\"")];
    args.extend(
        spec.arguments()
            .iter()
            .map(|s| format!("\"{}\"", s.replace('"', "\\\""))),
    );
    ensure!(
        !bin.contains(['\n', '\r', '"'])
            && !spec.directory.to_string_lossy().contains(['\n', '\r', '"']),
        "Ruta inválida"
    );
    let env_path = spec.directory.join("service-env.json");
    let env_json = serde_json::to_string(&spec.environment)?;
    elevate(&format!(
        r#"
$binPath={command}
$svc=Get-CimInstance Win32_Service -Filter "Name='{NAME}'"
if($svc) {{
  Stop-Service '{NAME}' -Force
  $changed=Invoke-CimMethod -InputObject $svc -MethodName Change -Arguments @{{PathName=$binPath;StartMode='Automatic';StartName='NT AUTHORITY\LocalService'}}
  if($changed.ReturnValue -ne 0) {{ throw 'No se pudo actualizar el servicio' }}
}} else {{
  $credential=[PSCredential]::new('NT AUTHORITY\LocalService',(New-Object Security.SecureString))
  New-Service -Name '{NAME}' -BinaryPathName $binPath -StartupType Automatic -Credential $credential | Out-Null
}}
& icacls.exe {binary} /inheritance:r /grant:r '*S-1-5-19:RX' '*S-1-5-32-544:F' '*S-1-5-18:F' {owner} | Out-Null
& icacls.exe {directory} /inheritance:r /grant:r '*S-1-5-19:(OI)(CI)M' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' {caller} | Out-Null
if($LASTEXITCODE -ne 0) {{ throw 'No se pudieron proteger los archivos' }}
[IO.File]::WriteAllText({env_path},{env_json},(New-Object Text.UTF8Encoding($false)))
& sc.exe failure '{NAME}' reset= 86400 actions= restart/3000/restart/10000/restart/30000 | Out-Null
Start-Service '{NAME}'
"#,
        command = ps(&args.join(" ")),
        binary = ps(&bin),
        directory = ps(&spec.directory.to_string_lossy()),
        env_path = ps(&env_path.to_string_lossy()),
        env_json = ps(&env_json)
    ))
}
pub fn manage(action: &Action) -> Result<()> {
    match action {
        Action::Start => elevate(&format!("Start-Service '{NAME}'")),
        Action::Stop => elevate(&format!("Stop-Service '{NAME}'")),
        Action::Restart => elevate(&format!("Restart-Service '{NAME}'")),
        Action::Status => {
            let mut c = Process::new("sc.exe");
            c.args(["query", NAME]);
            checked(c)
        }
        _ => unreachable!(),
    }
}
