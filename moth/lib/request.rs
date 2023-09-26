use super::{Sites, Arc, Endpoint, Site, ScriptCommand};
use flume::Sender;
use tiny_http::{Server, Request, Response};

pub fn request_waiter(
    server: Arc<Server>,
    runs_tx: Sender<ScriptCommand>,
    sites: Sites,
    tid: usize,
) {
    loop {
        let request = server.recv();
        if let Ok(request) = request {
            let mut site = None;

            for header in request.headers() {
                if header.field.equiv("Host") {
                    let host = header.value.as_str().split(":").next().unwrap();
                    site = sites.get(host);
                    break;
                }
            }

            if let Some(site) = site {
                let mut path_vars = Vec::new();
                let mut path_override = None;

                let mut endpoint = site.routes();
                let path_iter = request.url().split('/').filter(|s| !s.is_empty());
                for step in path_iter {
                    if let Endpoint::Upload = endpoint {
                        path_vars.push(step.into());
                        continue;
                    }

                    if let Endpoint::Dir(map) = endpoint {
                        if let Some(next) = map.items.get(step) {
                            endpoint = next;
                            continue;
                        }

                        if let Some(Endpoint::Static(_)) = map.default.as_deref() {
                            // fallback to empty
                            endpoint = map.default.as_ref().unwrap();
                            // must not continue/break so that the step path land in path_override
                        } else if let Some(next) = map.wildcard.as_deref() {
                            path_vars.push(step.into());
                            endpoint = &next;
                            continue;
                        }
                    }

                    if let Endpoint::Static(path) = endpoint {
                        let path = path_override.get_or_insert_with(|| path.to_string());
                        path.push_str("/");
                        path.push_str(step);

                        continue;
                    }

                    endpoint = site.on_404();
                    break;
                }

                while let Endpoint::Dir(map) = endpoint {
                    if let Some(next) = map.default.as_deref() {
                        endpoint = &next;
                        continue;
                    }

                    endpoint = site.on_404();
                    break;
                }

                process_endpoint(Some(&site), path_vars, path_override, request, endpoint, &runs_tx, tid);
            } else {
                log::error!("Unknown host in request header");
                process_endpoint(None, Vec::new(), None, request, &Endpoint::Error(502.into()), &runs_tx, tid);
            }
        } else if let Err(error) = request {
            log::error!("Error while parsing http request: {}", error);
        }
    }
}

fn process_endpoint(
    site: Option<&Arc<dyn Site>>,
    path_vars: Vec<String>,
    path_override: Option<String>,
    mut request: Request,
    endpoint: &Endpoint,
    runs_tx: &Sender<ScriptCommand>,
    tid: usize,
) {
    if let Endpoint::ScriptExec(read_only, script_name) = endpoint {
        let site = site.unwrap();
        let mut content = String::new();
        if let Ok(_) = request.as_reader().read_to_string(&mut content) {
            if let Ok(body) = site.parse_json(&content, tid) {
                let _ = runs_tx.send(ScriptCommand {
                    site: site.clone(),
                    script_name: script_name.clone(),
                    read_only: *read_only,
                    path_vars,
                    body,
                    request,
                });
            } else {
                log::error!("Couldn't parse request body as JSON");
                process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(400.into()), runs_tx, tid);
            }
        } else {
            log::error!("Couldn't read request body");
            process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(400.into()), runs_tx, tid);
        }
    } else if let Endpoint::Static(path) = endpoint {
        let site = site.unwrap();
        let path = path_override.as_deref().unwrap_or(path);

        if let Some(reader) = site.open_static(path) {
            let response = Response::new(200.into(), vec![], reader, None, None);
            if let Err(error) = request.respond(response) {
                log::error!("Couldn't respond: {:?}", error);
            }
        } else {
            log::error!("Missing static resource: {}", path);
            if site.on_404() != endpoint {
                process_endpoint(Some(site), Vec::new(), None, request, site.on_404(), runs_tx, tid);
            } else {
                log::error!("Invalid 404 handler");
                process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(500.into()), runs_tx, tid);
            }
        }
    } else if let Endpoint::Upload = endpoint {
        let site = site.unwrap();
        if path_vars.len() == 1 {
            let token = &path_vars[0];
            if let Some(mut body_len) = site.check_upload_token(token) {
                let chunk_size = 4096 * 4;

                let reader = request.as_reader();
                let mut buf = vec![0; chunk_size];

                while body_len > 0 {
                    if let Ok(len) = reader.read(&mut buf) {
                        site.upload_progress(token, &buf[..len]);
                        if let Some(len) = body_len.checked_sub(len) {
                            body_len = len;
                        } else {
                            site.end_of_upload(token, false);
                            log::error!("Client tried to upload more than allowed");
                            return process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(400.into()), runs_tx, tid);
                        }
                    } else {
                        site.end_of_upload(token, false);
                        log::error!("Failed to process upload request");
                        return process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(400.into()), runs_tx, tid);
                    }
                }

                site.end_of_upload(token, true);

                let response = "success".as_bytes();
                if let Err(error) = request.respond(Response::new(200.into(), vec![], response, None, None)) {
                    log::error!("Couldn't respond: {:?}", error);
                }

                return;
            }
        }

        log::error!("Invalid upload token/request");
        process_endpoint(Some(site), Vec::new(), None, request, &Endpoint::Error(400.into()), runs_tx, tid);
    } else if let Endpoint::Error(code) = endpoint {
        let body = include_str!("proc-failure.html").as_bytes();
        let response = Response::new(*code, vec![], body, None, None);
        if let Err(error) = request.respond(response) {
            log::error!("Couldn't respond: {:?}", error);
        }
    } else {
        log::error!("Landed at an Endpoint::Directory(_) without any wildcard route");
        process_endpoint(site, Vec::new(), None, request, &Endpoint::Error(500.into()), runs_tx, tid);
    }
}
