use rustgit::{create_ed25519_keypair, dump_ed25519_pk_openssh};
use lmfu::{ArcStr, json::{JsonFile, Path as JsonPath}};
use std::{env, io, fs, process::Command, path::Path};
use cpio::{NewcBuilder, write_cpio};
use ureq::post;

fn init_logger() {
    use simplelog::*;
    let config = ConfigBuilder::new().set_location_level(LevelFilter::Off).build();
    let _ = SimpleLogger::init(LevelFilter::Info, config);
}

const CPIO_REGULAR_FILE_MODE: u32 = 0o100_000;

fn keygen() {
    let keypair = create_ed25519_keypair();
    let openssh = dump_ed25519_pk_openssh(&keypair, "[username]");

    println!("keypair_hex: {}", keypair);
    println!("Public Key:  {}", openssh);
    println!("Replace '[username]' with your username on the GIT server.");
    println!("-> For GitHub: This is your login email");
    println!("");
    println!("Add this public key to `authorized_keys` on your GIT server");
    println!("-> For GitHub: https://github.com/settings/keys");
    println!("");
    println!("Use the keypair_hex string in your moth configuration file.");
    println!("Run with --help for more information.");
}

fn print_usage() {
    println!("Usage: cargo moth [OPTIONS] SITE_HOST DEPLOY_HOST");
    println!("Will build, bundle and upload a service to a running moth server");
    println!("");
    println!("OPTIONS:");
    println!("    -h, --help                      Print help and exit");
    println!("    -q, --quiet                     Do not print cargo log messages");
    println!("    -r, --release                   Build artifacts in release mode, with optimizations");
    println!("    -v, --verbose                   Use verbose output when building with cargo");
    println!("        --keygen                    Generate an SSH key pair and exit");
    println!("        --frozen                    Require Cargo.lock and cache are up to date");
    println!("        --locked                    Require Cargo.lock is up to date");
    println!("        --offline                   Run without accessing the network");
    println!("        --all-features              Activate all available features");
    println!("        --manifest-path <PATH>      Path to Cargo.toml");
    println!("        --dump-service BUNDLE_PATH  Dump the service bundle at BUNLDE_PATH");
    println!("");
    println!("This utility will use the default target building directory.");
    println!("This utility makes some assumptions about the service crate:");
    println!("    - The crate's [lib] output must be named 'site'.");
    println!("    - The crate must not be part of a workspace.");
    println!("    - The crate must contain be a 'bundle' directory.");
    println!("        - The bundle directory must contain a service configuration file (config.json).");
    println!("        - The bundle directory can contain any asset/subdir you want, such as templates");
    println!("          or other regular files and directories.");
    println!("");
    println!("The configuration file must be a valid JSON file with the following properties:");
    println!("    routes             The routes that this service allows");
    println!("    on_404             The routes that this service takes on HTTP error 404");
    println!("    database           Database access config for the service");
    println!("    |-- host           Git server; For GitHub: 'github.com:22'");
    println!("    |-- username       Git username; For GitHub: 'git'");
    println!("    |-- keypair_hex    Hex-Encoded key pair to use (generate one with --keygen)");
    println!("    |-- path           Git repository; Example: 'MyAccount/my-db-repo.git'");
    println!("    `-- branch         Git branch to use in the database GIT repository");
    println!("");
    println!("Format of routes & on_404 in the configuration file:");
    println!("    This part of the configuration file allows you to define endpoints");
    println!("    in the server. The path component of a request's URL will guide the");
    println!("    server in choosing what to respond with.");
    println!("");
    println!("    In this part of the configuration file:");
    println!("    - string values are used for static assets");
    println!("        - this can be a path to a directory in the bundle");
    println!("        - or to a specific file in the bundle");
    println!("    - arrays represent script callbacks:");
    println!("        - The first array item must be 'rw' or 'ro':");
    println!("            - 'ro': the script callbacks will get a read-only access to the database");
    println!("            - 'rw': the script callbacks will get a read-write access to the database");
    println!("        - The second array item is the name of the script callback (rust function name)");
    println!("    - objects represent directories");
    println!("        - directory objects can have special keys:");
    println!("        - [param]: can match any path item.");
    println!("        - [empty]: will match when the directory itself is accessed.");
    println!("                   Warning: when the value routed to this key is a static bundle");
    println!("                   directory, this allows free access, to all contained assets.");
    println!("        - other keys must match what is written exactly.");
    println!("");
    println!("    For example:");
    println!("");
    println!("    \"routes:\" {{");
    println!("        \"assets\": {{");
    println!("            \"[param]\": \"denied.png\"");
    println!("        }}");
    println!("    }}");
    println!("");
    println!("    This route will allow the following URLs:");
    println!("    - https://myhost/assets/wooooooo");
    println!("    - https://myhost/assets/reiubhrzegr");
    println!("    - https://myhost/assets/reiubhr");
    println!("    And they will all lead to bundle/denied.png");
    println!("");
    println!("    ...but will deny the following URL because it goes 'too far':");
    println!("    - https://myhost/assets/something/something");
    println!("");
    println!("    Please ask the authors directly for more information.");
}


fn main() {
    init_logger();

    let mut args = env::args();
    let mut cargo_args = vec!["build", "--target=wasm32-unknown-unknown"];
    let mut pos_args = Vec::new();
    let mut cpio_dump = None;
    let mut manifest_path = "./Cargo.toml".into();
    let cargo = env::var("CARGO");
    let cargo = cargo.as_deref().unwrap_or("cargo");

    let program = args.next().expect("Missing program path");
    log::debug!("program: {}", program);

    let forward_list = ["release", "quiet", "verbose", "frozen", "locked", "offline", "all-features"];

    while let Some(arg) = args.next() {
        if arg == "--help" {
            return print_usage();
        } else if arg == "--keygen" {
            return keygen();
        } else if arg == "--dump-service" {
            let path = args.next().expect("Missing path following --dump-service");
            cpio_dump = Some(path);
        } else if arg == "--manifest-path" {
            let path = args.next().expect("Missing path following --manifest-path");
            manifest_path = path;
        } else if let Some(path) = arg.strip_prefix("--manifest-path=") {
            manifest_path = path.into();
        } else if let Some(switch) = arg.strip_prefix("--") {
            if let Some(switch) = forward_list.iter().find(|s| *s == &switch) {
                cargo_args.push(switch);
            } else {
                return println!("Unexpected/Unsupported switch/option: {}", switch)
            }
        } else if let Some(switches) = arg.strip_prefix("-") {
            for switch in switches.chars() {
                match switch {
                    'r' => cargo_args.push("--release"),
                    'q' => cargo_args.push("--quiet"),
                    'v' => cargo_args.push("--verbose"),
                    'h' => return print_usage(),
                    s => return println!("Unexpected/Unsupported switch: {}", s),
                }
            }
        } else {
            pos_args.push(arg);
        }
    }

    let deploy_host = pos_args.pop().expect("Missing positional argument: DEPLOY_HOST");
    let site_host = pos_args.pop().expect("Missing positional argument: SITE_HOST");

    let profile = match cargo_args.contains(&"--release") {
        true => "release",
        false => "debug",
    };

    cargo_args.push("--manifest-path");
    cargo_args.push(&manifest_path);

    // ------------------ STEP 1 ------------------
    println!("Building Service Callbacks");

    let err = "Failed to run cargo build";
    Command::new(cargo)
            .env("RUSTFLAGS", "-C strip=symbols")
            .args(&cargo_args)
            .spawn()
            .expect(err)
            .wait()
            .expect(err);

    // ------------------ STEP 2 ------------------
    println!("Creating Service Bundle In Memory");

    let path = Path::new(&manifest_path).parent().expect("Invalid manifest path");
    let bundle = path.join("bundle");

    let header = |path: &str| NewcBuilder::new(path).mode(CPIO_REGULAR_FILE_MODE);

    // todo: guess binary name from manifest
    println!("- Bundling site.wasm");
    let site_wasm_path = path.join(format!("target/wasm32-unknown-unknown/{}/site.wasm", profile));
    let site_wasm = match fs::File::open(&site_wasm_path) {
        Ok(file) => file,
        Err(e) => return println!("Failed to open {}: {}", site_wasm_path.display(), e),
    };

    let mut to_bundle = vec![(header("site.wasm"), site_wasm)];
    let mut seen_config_json = false;

    let mut process_bundle_entry = |path: &Path| {
        let bundle_path = path.strip_prefix(&bundle).ok().and_then(|bp| bp.to_str());
        if let Some(bundle_path) = bundle_path {
            if bundle_path == "config.json" {
                seen_config_json = true;
            }

            println!("- Bundling {}", bundle_path);
            let file = match fs::File::open(&path) {
                Ok(file) => file,
                Err(e) => return println!("Failed to open {}: {}", path.display(), e),
            };
            to_bundle.push((header(bundle_path), file));
        } else {
            return println!("Failed to process {}", path.display());
        }
    };

    if let Err(e) = visit_dirs(&bundle, &mut process_bundle_entry) {
        log::error!("{:?}", e);
        return println!("Failed to open bundle directory: {:?}", bundle);
    }

    if !seen_config_json {
        return println!("> Bundle: Missing config.json");
    }

    let mut bundle = Vec::new();
    match write_cpio(to_bundle.into_iter(), &mut bundle) {
        Ok(_) => (),
        Err(e) => return println!("Failed to create bundle: {}", e),
    };

    println!("> Created bundle successfully: {} bytes", bundle.len());

    if let Some(path) = cpio_dump {
        fs::write(path, &bundle).expect("Failed to dump service archive");
    }

    // ------------------ STEP 3 ------------------
    println!("Requesting Service Upload");

    let mut file = JsonFile::new(None).unwrap();
    file.set_object(&JsonPath::new());
    let mut set = |prop, string: ArcStr| {
        let path = file.prop(JsonPath::new(), prop);
        file.set_string(&path, string);
    };

    set("site", site_host.into());
    set("key", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".into());
    set("size_bytes", format!("{}", bundle.len()).into());

    let payload = file.dump(&JsonPath::new()).unwrap();

    let request_url = format!("http://{}/request", deploy_host);
    let resp = match post(&request_url).send(payload.as_bytes()) {
        Ok(resp) => resp.into_string().unwrap(),
        Err(e) => return println!("Failed to request service upload: {:?}", e),
    };

    let err = "Invalid server reply";
    let resp = JsonFile::new(Some(&resp)).expect(err);
    let token = resp.get(&JsonPath::new()).as_string().expect(err);

    println!("token: {}", token);

    // ------------------ STEP 4 ------------------
    println!("Uploading Service");

    let upload_url = format!("http://{}/upload/{}", deploy_host, token);
    let resp = match post(&upload_url).send(bundle.as_slice()) {
        Ok(resp) => resp.into_string().unwrap(),
        Err(e) => return println!("Failed to request service upload: {:?}", e),
    };

    let msg = match resp == "success" {
        true => "> Service uploaded successfully",
        false => "> Failed to upload service",
    };

    println!("{}", msg);
}

fn visit_dirs<F: FnMut(&Path)>(dir: &Path, cb: &mut F) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit_dirs(&path, cb)?;
        } else {
            cb(&path);
        }
    }

    Ok(())
}
