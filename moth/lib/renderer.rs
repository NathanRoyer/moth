use super::{PoolStr, OpaqueJsonPointer, Arc, Site};
use tiny_http::{Request, Response};
use flume::Receiver;
use lmfu::LiteMap;

pub enum RendererCommand {
    Template {
        site: Arc<dyn Site>,
        template: PoolStr,
        parameters: LiteMap<PoolStr, String>,
    },
    Json {
        site: Arc<dyn Site>,
        json_body: OpaqueJsonPointer,
    },
}

pub fn renderer(
    renders_rx: Receiver<(Request, RendererCommand)>,
    tid: usize,
) {
    for (request, command) in renders_rx.into_iter() {
        let result = match command {
            RendererCommand::Template {
                site,
                template,
                parameters,
            } => site.render_template(template, parameters),
            RendererCommand::Json {
                site,
                json_body,
            } => site.dump_json(json_body, tid),
        };

        let respond = |reader, code: u32| {
            let response = Response::new(code.into(), vec![], reader, None, None);
            if let Err(error) = request.respond(response) {
                log::error!("Couldn't respond: {:?}", error);
            }
        };

        match result {
            Ok(body) => respond(body.as_bytes(), 200),
            Err(()) => respond(b"Renderer error".as_slice(), 500),
        }
    }
}
