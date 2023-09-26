use moth::{serve, Site, Sites, ScriptResult, OpaqueJsonPointer, Endpoint, EndpointMap};
use lmfu::json::{JsonFile, Value as JsonValue, Path as JsonPath};
use rustgit::{Remote, Repository, Reference, Error as GitError};
use std::{sync::{Arc, RwLock, Mutex}, io::Read, env::args};
use lmfu::strpool::{Pool, PoolStr};
use upon::{Engine as UponEngine};
use lmfu::{LiteMap, HashMap};
use core::str::from_utf8;
use cpio::NewcReader;

mod wasm;
mod handle;
mod deploy;

use wasm::WasmThread;
use handle::{Handle, TemplateParams};
use deploy::Deployer;

fn init_logger() {
    use simplelog::*;
    let config = ConfigBuilder::new().set_location_level(LevelFilter::Off).build();
    let _ = SimpleLogger::init(LevelFilter::Info, config);
}

struct WasmApp {
    pool: Pool,
    name: PoolStr,
    domain: PoolStr,
    routes: Endpoint,
    on_404: Endpoint,
    upon_engine: UponEngine<'static>,
    threads: RwLock<Vec<Mutex<WasmThread>>>,
    assets: HashMap<str, Box<[u8]>>,
    repo: RwLock<Arc<RwLock<Repository>>>,
    #[allow(dead_code)]
    db_remote: Remote,
}

impl Site for WasmApp {
    fn pool(&self) -> &Pool { &self.pool }
    fn name(&self) -> &str { &self.name }
    fn hostname(&self) -> &str { &self.domain }
    fn routes(&self) -> &Endpoint { &self.routes }
    fn on_404(&self) -> &Endpoint { &self.on_404 }

    fn check_upload_token(&self, _token: &str) -> Option<usize> {
        /*todo*/ None
    }

    fn upload_progress(&self, _token: &str, _to_append: &[u8]) {
        /*todo*/
    }

    fn end_of_upload(&self, _token: &str, _success: bool) {
        /*todo*/
    }

    fn render_template(&self, name: PoolStr, parameters: LiteMap<PoolStr, String>) -> Result<String, ()> {
        /*let asset = match self.assets.get(&name) {
            Some(asset) => Ok(asset),
            None => Err(log::error!("Missing template: {}", name)),
        }?;

        let template_src = match core::str::from_utf8(asset) {
            Ok(template) => Ok(template),
            Err(_) => Err(log::error!("Invalid bytes in template: {}", name)),
        }?;*/

        let template = match self.upon_engine.compile("Hello {{ user.name }}!") {
            Ok(template) => Ok(template),
            Err(e) => Err(log::error!("Failed to compile template {}: {}", name, e)),
        }?;

        let renderer = template.render_from_fn(|members| {
            use upon::*;

            if members.len() == 1 {
                let ValueMember { op, access } = members[0];
                if let (ValueAccessOp::Direct, ValueAccess::Key(key)) = (op, access) {
                    match parameters.get_by(|probe| (**probe).cmp(key)) {
                        Some(string) => Ok(Value::String(string.clone())),
                        None => Err(format!("Unknown template parameter: {}", key)),
                    }
                } else {
                    Err("Invalid access: only direct references are supported".into())
                }
            } else {
                Err("Invalid access: only direct references are supported".into())
            }
        });

        match renderer.to_string() {
            Ok(output) => Ok(output),
            Err(e) => Err(log::error!("Failed to render template {}: {}", name, e)),
        }
    }

    fn open_static(&self, path: &str) -> Option<&[u8]> {
        self.assets.get(path).map(|a| &**a)
    }

    fn prepare_tls(&self, script_threads: usize) {
        let mut threads = self.threads.write().unwrap();

        while threads.len() < script_threads {
            let new_thread = {
                let first_thread = threads[0].lock().unwrap();
                Mutex::new(first_thread.clone())
            };
            threads.push(new_thread);
        }
    }

    fn parse_json(&self, json: &str, thread_index: usize) -> Result<OpaqueJsonPointer, ()> {
        let threads = self.threads.read().unwrap();
        let mut thread = threads[thread_index].lock().unwrap();

        match thread.parse_json(json) {
            Ok(opaq_ptr) => Ok(opaq_ptr),
            Err(trap) => Err(log::error!("{}", trap)),
        }
    }

    fn dump_json(&self, json: OpaqueJsonPointer, thread_index: usize) -> Result<String, ()> {
        let threads = self.threads.read().unwrap();
        let mut thread = threads[thread_index].lock().unwrap();

        match thread.dump_json(json) {
            Ok(string) => Ok(string),
            Err(trap) => Err(log::error!("{}", trap)),
        }
    }

    fn process_script(
        &self,
        script: PoolStr,
        read_only: bool,
        path_vars: &[String],
        body: OpaqueJsonPointer,
        thread_index: usize,
    ) -> Result<ScriptResult, ()> {
        let threads = self.threads.read().unwrap();
        let mut thread = threads[thread_index].lock().unwrap();

        let db_token = 0;
        let result = thread.call_script_fn(&*script, read_only, &self.repo, db_token, body, path_vars);
        let script_result = match result {
            Ok(script_result) => script_result,
            Err(trap) => return Err(log::error!("{}", trap)),
        };

        match script_result {
            (None, Some(json_ptr)) => Ok(ScriptResult::Json(json_ptr)),
            (Some((template, parameters)), None) => Ok(ScriptResult::Template { template, parameters }),
            (_, _) => Err(()),
        }
    }
}

impl WasmApp {
    pub fn new(cpio: &[u8], hostname: &str) -> Result<Self, ()> {
        let mut site_wasm = None;
        let mut config_json = None;
        let pool = Pool::new();
        let mut assets: HashMap<str, Box<[u8]>> = HashMap::new();

        let mut file = cpio;
        loop {
            let mut reader = NewcReader::new(file).map_err(|_| log::error!("Invalid CPIO archive"))?;
            if reader.entry().is_trailer() {
                break;
            }

            let size = reader.entry().file_size() as usize;
            let read_content = |reader: &mut NewcReader<_>| {
                let mut content = Vec::with_capacity(size);
                assert_eq!(reader.read_to_end(&mut content).ok(), Some(size));
                content.into_boxed_slice()
            };

            match reader.entry().name() {
                "site.wasm" => site_wasm = Some(read_content(&mut reader)),
                "config.json" => config_json = Some(read_content(&mut reader)),
                _ => {
                    let content = read_content(&mut reader);
                    assets.insert_ref(reader.entry().name(), content);
                },
            }
            file = reader.finish().map_err(|_| log::error!("Invalid CPIO archive"))?;
        }

        let config_json = match config_json {
            Some(json) => Ok(json),
            None => Err(log::error!("no config.json"))
        }?;

        let config_json = match from_utf8(&config_json) {
            Ok(json) => Ok(json),
            Err(_) => Err(log::error!("Invalid bytes in config.json")),
        }?;

        let config = match JsonFile::with_key_pool(Some(config_json), pool.clone()) {
            Ok(config) => Ok(config),
            Err(e) => Err(log::error!("Invalid config.json: {:?}", e)),
        }?;

        let routes = parse_routes(&config, &pool, &JsonPath::new().i_str("routes"))?;
        let on_404 = parse_routes(&config, &pool, &JsonPath::new().i_str("on_404"))?;

        let db_path = JsonPath::new().i_str("database");

        let db_remote = match Remote::parse(&config, &db_path) {
            Ok(db_remote) => Ok(db_remote),
            Err(e) => Err(log::error!("Invalid remote database access config: {:?}", e)),
        }?;

        let branch = match config.get(&db_path.i_str("branch")).as_string() {
            Some(string) => Ok(string),
            None => Err(log::error!("Invalid branch config: must be a string")),
        }?;

        let mut repo = Repository::new();

        // quick bypass toggle
        if true {
            match repo.clone(&db_remote, Reference::Branch(branch), Some(1)) {
                Ok(()) | Err(GitError::NoSuchReference) => Ok(()),
                Err(e) => Err(log::error!("Failed to clone database: {:?}", e)),
            }?;
        }

        let wasm_thread = match site_wasm {
            Some(site_wasm) => match WasmThread::new(&site_wasm, pool.clone()) {
                Some(wasm_thread) => Ok(wasm_thread),
                None => Err(log::error!("Failed to instantiate site.wasm")),
            },
            None => Err(log::error!("no site.wasm")),
        }?;

        let domain = pool.intern(hostname);
        let name = domain.clone();

        Ok(WasmApp {
            pool,
            name,
            domain,
            routes,
            on_404,
            upon_engine: UponEngine::new(),
            threads: RwLock::new(vec![Mutex::new(wasm_thread)]),
            assets,
            repo: RwLock::new(Arc::new(RwLock::new(repo))),
            db_remote,
        })
    }
}

fn main() {
    let pool = Pool::get_static_pool();

    let filename = args().last();
    let filename = filename.as_deref().unwrap_or("-h");

    if filename == "-h" || filename == "--help" {
        println!("Usage:");
        println!("    moth config.json     Start the server with a configuration file");
        println!("    moth -h/--help       Print this usage info");
        println!("");
        println!("The configuration file must be a valid JSON file with the following properties:");
        println!("    request_threads      Number of threads handling incoming requests");
        println!("    script_threads       Number of threads handling script executions");
        println!("    render_threads       Number of threads handling template renderings");
        println!("    max_service_cpio_mb  Maximum file of uploaded service bundles");
        println!("    hostname             Hostname for the deployment service");
        println!("    listen_addr          Listening address (example: 0.0.0.0:80)");

        return;
    }

    let config = match std::fs::read_to_string(&filename) {
        Ok(file) => file,
        Err(_) => panic!("Failed to read config file {}", filename),
    };

    let config = match JsonFile::with_key_pool(Some(&config), pool) {
        Ok(file) => file,
        Err(e) => panic!("Failed to parse config file: {}", e),
    };

    let get = |prop| config.get(&JsonPath::new().i_str(prop));
    let get_num = |prop| match get(prop).as_num() {
        Some(value) => value as usize,
        None => panic!("Invalid property '{}' in config file", prop),
    };
    let get_str = |prop| match get(prop).as_string() {
        Some(string) => string.clone(),
        None => panic!("Invalid property '{}' in config file", prop),
    };

    const MB: usize = 1024 * 1024;
    let request_threads = get_num("request_threads");
    let script_threads = get_num("request_threads");
    let render_threads = get_num("render_threads");
    let upload_limit = get_num("max_service_cpio_mb") * MB;
    let hostname = get_str("hostname");
    let listen_addr = get_str("listen_addr");

    init_logger();

    let sites = Sites::new(request_threads, script_threads, render_threads);
    let deployer = Deployer::new(hostname, upload_limit, sites.clone());
    sites.insert(Box::new(deployer));

    serve(listen_addr.as_str(), sites);
}

fn parse_routes(file: &JsonFile, pool: &Pool, path: &JsonPath) -> Result<Endpoint, ()> {
    match file.get(path) {
        JsonValue::Array(length) => {
            if *length != 2 {
                return Err(log::error!("Invalid route (array != 2 items)"));
            }

            // access (rw/ro)
            let read_only = match file.get(&path.clone().i_num(0)) {
                JsonValue::String(s) if s == "ro" => true,
                JsonValue::String(s) if s == "rw" => false,
                _ => return Err(log::error!("Invalid route (access must be ro/rw)")),
            };

            let fn_name = match file.get(&path.clone().i_num(1)) {
                JsonValue::String(fn_name) => pool.intern(fn_name),
                _ => return Err(log::error!("Invalid route (function name must be a string)")),
            };

            Ok(Endpoint::ScriptExec(read_only, fn_name))
        },
        JsonValue::Object(keys) => {
            let mut items = HashMap::new();
            let mut default = None;
            let mut wildcard = None;

            for key in keys {
                let sub_path = path.clone().i_str(&key);
                let value = parse_routes(file, pool, &sub_path)?;
                match &**key {
                    "[param]" => wildcard = Some(Box::new(value)),
                    "[empty]" => default = Some(Box::new(value)),
                    key => { items.insert_ref(key, value); },
                }
            }

            Ok(Endpoint::Dir(EndpointMap {
                default,
                wildcard,
                items,
            }))
        },
        JsonValue::String(endpoint_path) => match endpoint_path.as_str() {
            "[upload]" => Ok(Endpoint::Upload),
            path => Ok(Endpoint::Static(pool.intern(&path))),
        },
        JsonValue::Number(_) => Err(log::error!("Invalid route (number)")),
        JsonValue::Boolean(_) => Err(log::error!("Invalid route (true/false)")),
        JsonValue::Null => Err(log::error!("Invalid route (null)")),
    }
}
