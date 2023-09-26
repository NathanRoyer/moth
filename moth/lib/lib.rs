// #![doc = include_str!("../../README.md")]

use std::{sync::{Arc, RwLock}, thread, net::ToSocketAddrs};
use lmfu::{strpool::{Pool, PoolStr}, LiteMap, HashMap};
use tiny_http::{Server, StatusCode};

pub type OpaqueJsonPointer = usize;

pub mod request;
pub mod script;
pub mod renderer;

pub use {
    request::{request_waiter},
    script::{script_runner, ScriptCommand, ScriptResult},
    renderer::{renderer, RendererCommand},
};

#[derive(Debug, PartialEq)]
pub struct EndpointMap {
    pub default: Option<Box<Endpoint>>,
    pub wildcard: Option<Box<Endpoint>>,
    pub items: HashMap<str, Endpoint>,
}

pub type ReadOnly = bool;

#[derive(Debug, PartialEq)]
pub enum Endpoint {
    ScriptExec(ReadOnly, PoolStr),
    Static(PoolStr),
    Dir(EndpointMap),
    Upload,
    Error(StatusCode),
}

pub trait Site: Sync + Send + 'static {
    fn pool(&self) -> &Pool;
    fn name(&self) -> &str;
    fn hostname(&self) -> &str;
    fn prepare_tls(&self, script_threads: usize);

    fn parse_json(&self, json: &str, script_thread_id: usize) -> Result<OpaqueJsonPointer, ()>;
    fn dump_json(&self, json: OpaqueJsonPointer, script_thread_id: usize) -> Result<String, ()>;

    fn on_404(&self) -> &Endpoint;
    fn routes(&self) -> &Endpoint;

    fn open_static(&self, path: &str) -> Option<&[u8]>;

    fn check_upload_token(&self, token: &str) -> Option<usize>;
    fn upload_progress(&self, token: &str, to_append: &[u8]);
    fn end_of_upload(&self, token: &str, success: bool);

    fn process_script(
        &self,
        script: PoolStr,
        read_only: bool,
        path_vars: &[String],
        body: OpaqueJsonPointer,
        script_thread_id: usize,
    ) -> Result<ScriptResult, ()>;

    fn render_template(&self, name: PoolStr, parameters: LiteMap<PoolStr, String>) -> Result<String, ()>;
}

#[derive(Clone)]
pub struct Sites {
    sites: Arc<RwLock<HashMap<str, Arc<dyn Site>>>>,
    request_threads: usize,
    script_threads: usize,
    render_threads: usize,
}

impl Sites {
    pub fn new(request_threads: usize, script_threads: usize, render_threads: usize) -> Self {
        Self {
            sites: Arc::new(RwLock::new(HashMap::new())),
            request_threads,
            script_threads,
            render_threads,
        }
    }

    pub(crate) fn total_threads(&self) -> usize {
        self.request_threads + self.script_threads + self.render_threads
    }

    pub fn insert(&self, site: Box<dyn Site>) {
        let mut map = self.sites.write().unwrap();
        site.prepare_tls(self.total_threads());
        let arc: Arc<dyn Site> = site.into();
        let clone = arc.clone();
        println!("Inserting site: {}", clone.hostname());
        map.insert_ref(clone.hostname(), arc);
    }

    pub(crate) fn get(&self, host: &str) -> Option<Arc<dyn Site>> {
        let map = self.sites.read().unwrap();
        map.get(host).cloned()
    }
}

pub fn serve<A: ToSocketAddrs>(addr: A, sites: Sites) {
    let server = Server::http(addr).unwrap();
    let server = Arc::new(server);

    let (runs_tx, runs_rx) = flume::unbounded();
    let (renders_tx, renders_rx) = flume::unbounded();

    let mut guards = Vec::with_capacity(sites.total_threads());

    for tid in 0..sites.request_threads {
        let (runs_tx, server, sites) = (runs_tx.clone(), server.clone(), sites.clone());
        let thread = thread::spawn(move || request_waiter(server, runs_tx, sites, tid));
        guards.push(thread);
    }

    for tid in 0..sites.script_threads {
        let tid = sites.request_threads + tid;
        let (runs_rx, renders_tx) = (runs_rx.clone(), renders_tx.clone());
        let thread = thread::spawn(move || script_runner(runs_rx, renders_tx, tid));
        guards.push(thread);
    }

    for tid in 0..sites.render_threads {
        let tid = sites.request_threads + sites.script_threads + tid;
        let renders_rx = renders_rx.clone();
        let thread = thread::spawn(move || renderer(renders_rx, tid));
        guards.push(thread);
    }

    for guard in guards {
        let _ = guard.join();
    }
}
