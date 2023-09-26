use super::{PoolStr, OpaqueJsonPointer, Site, Arc, RendererCommand};
use flume::{Receiver, Sender};
use tiny_http::Request;
use lmfu::LiteMap;

pub struct ScriptCommand {
    pub site: Arc<dyn Site>,
    pub script_name: PoolStr,
    pub read_only: bool,
    pub path_vars: Vec<String>,
    pub body: OpaqueJsonPointer,
    pub request: Request,
}

pub enum ScriptResult {
    Template {
        template: PoolStr,
        parameters: LiteMap<PoolStr, String>,
    },
    Json(OpaqueJsonPointer),
}

pub fn script_runner(
    runs_rx: Receiver<ScriptCommand>,
    renders_tx: Sender<(Request, RendererCommand)>,
    tid: usize,
) {
    for cmd in runs_rx.into_iter() {
        let site = cmd.site;
        let result = site.process_script(cmd.script_name, cmd.read_only, &cmd.path_vars, cmd.body, tid);
        if let Ok(script_result) = result {
            let render = match script_result {
                ScriptResult::Template { template, parameters } => RendererCommand::Template { site, template, parameters },
                ScriptResult::Json(json_body) => RendererCommand::Json { site, json_body },
            };
            let _ = renders_tx.send((cmd.request, render));
        }
    }
}
