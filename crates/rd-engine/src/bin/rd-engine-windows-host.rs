#[cfg(any(target_os = "windows", dsh_windows_typecheck))]
mod windows {
    use hbb_common::{
        base64::{Engine as _, engine::general_purpose::STANDARD},
        sodiumoxide::crypto::sign,
    };
    use rd_engine::{
        host::{Host, HostOptions},
        publisher::{CodecSelection, PublisherBackend, probe_display},
        windows_native::enumerate_outputs,
    };
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        net::SocketAddr,
        path::PathBuf,
        time::{Duration, Instant},
    };

    const IDENTITY_MAGIC: &[u8; 8] = b"RDEID001";

    struct Cli {
        listen: SocketAddr,
        id: String,
        backend: PublisherBackend,
        output: usize,
        codec: CodecSelection,
        fps: u32,
        bitrate: i64,
        test_auto_approve: bool,
        identity: Option<PathBuf>,
        list_outputs: bool,
    }
    impl Default for Cli {
        fn default() -> Self {
            Self {
                listen: "0.0.0.0:21118".parse().unwrap(),
                id: "rd-engine-windows-host".into(),
                backend: PublisherBackend::Auto,
                output: 0,
                codec: CodecSelection::Auto,
                fps: 60,
                bitrate: 20_000_000,
                test_auto_approve: false,
                identity: None,
                list_outputs: false,
            }
        }
    }
    fn value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("missing value for {name}"))
    }
    fn parse() -> Result<Cli, String> {
        let mut cli = Cli::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => {
                    cli.listen = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "invalid --listen".to_string())?
                }
                "--id" => cli.id = value(&mut args, &arg)?,
                "--backend" => {
                    cli.backend = match value(&mut args, &arg)?.as_str() {
                        "auto" => PublisherBackend::Auto,
                        "dxgi-nvenc" => PublisherBackend::DxgiNvenc,
                        _ => return Err("--backend must be auto or dxgi-nvenc".into()),
                    }
                }
                "--output" => {
                    cli.output = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "invalid --output".to_string())?
                }
                "--codec" => {
                    cli.codec = match value(&mut args, &arg)?.as_str() {
                        "auto" => CodecSelection::Auto,
                        "h264" => CodecSelection::H264,
                        "h265" => CodecSelection::H265,
                        _ => return Err("--codec must be auto, h264, or h265".into()),
                    }
                }
                "--fps" => {
                    cli.fps = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "invalid --fps".to_string())?
                }
                "--bitrate" => {
                    cli.bitrate = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "invalid --bitrate".to_string())?
                }
                "--identity" => cli.identity = Some(value(&mut args, &arg)?.into()),
                "--list-outputs" => cli.list_outputs = true,
                "--test-auto-approve" => cli.test_auto_approve = true,
                "--help" | "-h" => {
                    println!(
                        "rd-engine-windows-host [--listen ADDR] [--id ID] [--identity PATH] [--backend auto|dxgi-nvenc] [--list-outputs] [--output INDEX] [--codec auto|h264|h265] [--fps N] [--bitrate BPS] [--test-auto-approve]"
                    );
                    println!("stdin commands: approve REQUEST_ID, deny REQUEST_ID, quit");
                    std::process::exit(0);
                }
                _ => return Err(format!("unknown argument: {arg}")),
            }
        }
        Ok(cli)
    }

    fn default_identity_path() -> Result<PathBuf, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot resolve executable path: {error}"))?;
        Ok(executable.with_file_name("rd-engine-windows-host.identity"))
    }

    fn load_or_create_identity(
        path: &PathBuf,
    ) -> Result<(sign::PublicKey, sign::SecretKey), String> {
        match File::open(path) {
            Ok(mut file) => {
                let mut bytes = [0_u8; 104];
                file.read_exact(&mut bytes)
                    .map_err(|error| format!("cannot read identity file: {error}"))?;
                let mut extra = [0_u8; 1];
                if file
                    .read(&mut extra)
                    .map_err(|error| format!("cannot validate identity file: {error}"))?
                    != 0
                    || &bytes[..8] != IDENTITY_MAGIC
                {
                    bytes.fill(0);
                    return Err("identity file has an invalid format".into());
                }
                let public = sign::PublicKey::from_slice(&bytes[8..40])
                    .ok_or_else(|| "identity public key has an invalid length".to_string())?;
                let secret = sign::SecretKey::from_slice(&bytes[40..104])
                    .ok_or_else(|| "identity secret key has an invalid length".to_string())?;
                if secret.0[32..] != public.0 {
                    bytes.fill(0);
                    return Err("identity key pair is inconsistent".into());
                }
                bytes.fill(0);
                Ok((public, secret))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("cannot create identity directory: {error}"))?;
                }
                let (public, secret) = sign::gen_keypair();
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|error| format!("cannot create identity file: {error}"))?;
                file.write_all(IDENTITY_MAGIC)
                    .and_then(|_| file.write_all(&public.0))
                    .and_then(|_| file.write_all(&secret.0))
                    .and_then(|_| file.sync_all())
                    .map_err(|error| format!("cannot persist identity file: {error}"))?;
                Ok((public, secret))
            }
            Err(error) => Err(format!("cannot open identity file: {error}")),
        }
    }

    fn print_outputs() -> Result<(), String> {
        let outputs =
            enumerate_outputs().map_err(|error| format!("output enumeration failed: {error}"))?;
        if outputs.is_empty() {
            return Err("no attached desktop output was found".into());
        }
        for (index, output) in outputs.into_iter().enumerate() {
            println!(
                "output={} adapter_luid={} native_output={} name={:?} rect={},{},{},{} rotation={} color_space={} bits_per_color={} min_nits={} max_nits={} max_full_frame_nits={}",
                index,
                output.id.adapter_luid.0,
                output.id.output_index,
                output.device_name,
                output.desktop_rect[0],
                output.desktop_rect[1],
                output.desktop_rect[2],
                output.desktop_rect[3],
                output.rotation,
                output.display.raw_dxgi_color_space_type,
                output.display.bits_per_color,
                output.display.min_luminance_nits,
                output.display.max_luminance_nits,
                output.display.max_full_frame_luminance_nits
            );
        }
        Ok(())
    }

    pub async fn run() -> Result<(), String> {
        let cli = parse()?;
        if cli.list_outputs {
            return print_outputs();
        }
        let display = probe_display(cli.backend, cli.output)
            .map_err(|error| format!("display probe failed: {error}"))?;
        hbb_common::sodiumoxide::init().map_err(|_| "crypto initialization failed".to_string())?;
        let identity_path = match cli.identity.clone() {
            Some(path) => path,
            None => default_identity_path()?,
        };
        let (public_key, signing_key) = load_or_create_identity(&identity_path)?;
        eprintln!("host_id={} identity_file={:?}", cli.id, identity_path);
        eprintln!("peer_signing_key_base64={}", STANDARD.encode(public_key.0));
        let host = Host::start(HostOptions {
            listen: cli.listen,
            id: cli.id,
            signing_key,
            width: display.width,
            height: display.height,
            fps: cli.fps,
            bitrate: cli.bitrate,
            platform: "Windows".into(),
            publisher_backend: cli.backend,
            output_index: cli.output,
            codec_selection: cli.codec,
        })
        .map_err(|error| format!("host start failed: {error}"))?;

        let selected_backend = "dxgi-nvenc";
        eprintln!(
            "backend={selected_backend} fallback=none output={} geometry={}x{} fps={} codec={:?} listen={}",
            cli.output, display.width, display.height, cli.fps, cli.codec, cli.listen
        );
        eprintln!(
            "display_name={:?}; no pixel-copy or software fallback path is enabled",
            display.name
        );
        if cli.test_auto_approve {
            eprintln!("WARNING: test-only automatic approval is enabled explicitly");
        } else {
            eprintln!("approval required: enter 'approve REQUEST_ID' or 'deny REQUEST_ID'");
        }

        let (input_tx, input_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            loop {
                let mut line = String::new();
                let Ok(read) = stdin.read_line(&mut line) else {
                    break;
                };
                if read == 0 || input_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
        let mut last_phase = "";
        let mut last_request = String::new();
        let mut last_error = None;
        let mut last_metrics = Instant::now();
        loop {
            let snapshot = host.snapshot();
            if snapshot.phase != last_phase {
                eprintln!(
                    "phase={} connected={} encrypted={} codec={} fps={} sent_units={} sent_bytes={}",
                    snapshot.phase,
                    snapshot.connected,
                    snapshot.encrypted,
                    snapshot.codec,
                    snapshot.fps,
                    snapshot.sent_units,
                    snapshot.sent_bytes
                );
                last_phase = snapshot.phase;
            }
            if !snapshot.approval_request.is_empty() && snapshot.approval_request != last_request {
                eprintln!(
                    "approval_request={} origin={}",
                    snapshot.approval_request, snapshot.approval_origin
                );
                last_request = snapshot.approval_request.clone();
                if cli.test_auto_approve && !host.approve(&snapshot.approval_request, true) {
                    return Err("test auto-approval was rejected".into());
                }
            }
            while let Ok(line) = input_rx.try_recv() {
                let mut words = line.split_whitespace();
                match (words.next(), words.next(), words.next()) {
                    (Some("approve"), Some(request), None) => {
                        eprintln!("approval_applied={}", host.approve(request, true))
                    }
                    (Some("deny"), Some(request), None) => {
                        eprintln!("denial_applied={}", host.approve(request, false))
                    }
                    (Some("quit"), None, None) => {
                        return host
                            .close()
                            .await
                            .map_err(|error| format!("host close failed: {error}"));
                    }
                    (None, None, None) => {}
                    _ => eprintln!(
                        "invalid command; use approve REQUEST_ID, deny REQUEST_ID, or quit"
                    ),
                }
            }
            if snapshot.error != last_error {
                if let Some(error) = snapshot.error {
                    eprintln!("host_error={error}");
                }
                last_error = snapshot.error;
            }
            if snapshot.connected
                && snapshot.phase == "streaming"
                && last_metrics.elapsed() >= Duration::from_secs(5)
            {
                eprintln!(
                    "metrics fps={} sent_units={} sent_bytes={}",
                    snapshot.fps, snapshot.sent_units, snapshot.sent_bytes
                );
                last_metrics = Instant::now();
            }
            if snapshot.closed {
                if let Some(error) = snapshot.error {
                    return Err(format!("host stopped: {error}"));
                }
                return Ok(());
            }
            tokio::select! {
                result = &mut ctrl_c => {
                    result.map_err(|_| "failed to install Ctrl+C handler".to_string())?;
                    return host.close().await.map_err(|error| format!("host close failed: {error}"));
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }
}

#[cfg(any(target_os = "windows", dsh_windows_typecheck))]
#[tokio::main]
async fn main() {
    if let Err(error) = windows::run().await {
        eprintln!("rd-engine-windows-host: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(target_os = "windows", dsh_windows_typecheck)))]
fn main() {
    eprintln!("rd-engine-windows-host is available only on Windows");
    std::process::exit(1);
}
