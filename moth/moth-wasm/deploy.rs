use moth::{OpaqueJsonPointer, ScriptResult, Endpoint, EndpointMap, Site, Sites};
use lmfu::{ArcStr, LiteMap, HashMap, json::{JsonFile, Path as JsonPath}};
use super::{WasmApp, PoolStr, Pool};
use std::sync::{Mutex, RwLock};

type Key = [u8; 32];

type PendingUpload = Mutex<Vec<u8>>;

pub struct Deployer {
    pool: Pool,
    hostname: ArcStr,
    pending_uploads: RwLock<LiteMap<String, (PendingUpload, ArcStr)>>,
    admins: Mutex<HashMap<str, Key>>,
    sites: Sites,
    on_404: Endpoint,
    routes: Endpoint,
    max_size_bytes: usize,
}

impl Deployer {
    pub fn new(hostname: ArcStr, max_size_bytes: usize, sites: Sites) -> Self {
        let pool = Pool::new();
        let osef = pool.intern("_");
        let mut items = HashMap::new();

        items.insert_ref("upload", Endpoint::Upload);
        items.insert_ref("request", Endpoint::ScriptExec(false, osef.clone()));

        let routes = Endpoint::Dir(EndpointMap {
            default: None,
            wildcard: None,
            items,
        });

        Self {
            pool,
            hostname,
            pending_uploads: RwLock::new(LiteMap::new()),
            admins: Mutex::new(HashMap::new()),
            sites,
            on_404: Endpoint::Static(osef),
            routes,
            max_size_bytes,
        }
    }
}

impl Site for Deployer {
    fn pool(&self) -> &Pool { &self.pool }
    fn name(&self) -> &str { "[deployment server]" }
    fn hostname(&self) -> &str { &self.hostname }
    fn on_404(&self) -> &Endpoint { &self.on_404 }
    fn routes(&self) -> &Endpoint { &self.routes }
    fn render_template(&self, _name: PoolStr, _parameters: LiteMap<PoolStr, String>) -> Result<String, ()> { Err(()) }
    fn open_static(&self, _path: &str) -> Option<&[u8]> { Some(b"".as_slice()) }
    fn prepare_tls(&self, _script_threads: usize) {}

    fn parse_json(&self, json: &str, _script_thread_id: usize) -> Result<OpaqueJsonPointer, ()> {
        Ok(leak(Box::new(match JsonFile::new(Some(json)) {
            Ok(json) => json,
            Err(_e) => Err(())?
        })))
    }

    fn dump_json(&self, json: OpaqueJsonPointer, _script_thread_id: usize) -> Result<String, ()> {
        Ok(get_back(json).dump(&JsonPath::new()).unwrap().as_str().into())
    }

    fn check_upload_token(&self, token: &str) -> Option<usize> {
        let pending_uploads = self.pending_uploads.read().unwrap();
        if let Some((upload, _site)) = pending_uploads.get(token) {
            let bytes = upload.lock().unwrap();
            Some(bytes.capacity())
        } else {
            None
        }
    }

    fn upload_progress(&self, token: &str, to_append: &[u8]) {
        let pending_uploads = self.pending_uploads.read().unwrap();
        let (upload, _site) = pending_uploads.get(token).unwrap();
        let mut bytes = upload.lock().unwrap();
        bytes.extend_from_slice(to_append);
    }

    fn end_of_upload(&self, token: &str, success: bool) {
        let mut pending_uploads = self.pending_uploads.write().unwrap();
        if success {
            let (mut upload, hostname) = pending_uploads.remove(token).unwrap();
            core::mem::drop(pending_uploads);

            let bytes = upload.get_mut().unwrap();
            if let Ok(site) = WasmApp::new(&bytes, &hostname) {
                self.sites.insert(Box::new(site));
            } else {
                // constructor will have logged the error already
            }
        } else {
            let (upload, _site) = pending_uploads.get(token).unwrap();
            let mut bytes = upload.lock().unwrap();
            bytes.clear();
        }
    }

    fn process_script(
        &self, _script: PoolStr, _read_only: bool, _path_vars: &[String],
        body: OpaqueJsonPointer, _script_thread_id: usize,
    ) -> Result<ScriptResult, ()> {
        let params = get_back(body);
        let get = |prop| params.get(&JsonPath::new().i_str(prop));
        let get_str = |prop| get(prop).as_string().ok_or_else(|| log::error!("Invalid {} in upload request", prop));
        let get_hex = |prop| decode_hex(get_str(prop)?).ok_or_else(|| log::error!("Invalid {} in upload request", prop));

        let site = get_str("site")?;
        let submitted_key = get_hex("key")?;
        let size_bytes: usize = get_str("size_bytes")?.parse().map_err(|_| log::error!("Invalid size_bytes in upload request"))?;

        if size_bytes > self.max_size_bytes {
            return Err(log::error!("Service CPIO is too big"));
        }

        let mut admins = self.admins.lock().unwrap();
        if let Some(key) = admins.get(site) {
            if *key != submitted_key {
                return Err(log::error!("Invalid signature"));
            }
        } else {
            admins.insert_ref(site, submitted_key);
        }
        core::mem::drop(admins);

        let upload = Mutex::new(Vec::with_capacity(size_bytes));
        let mut pending_uploads = self.pending_uploads.write().unwrap();
        let token = loop {
            let number: u64 = rand::random();
            let token = format!("{:x}", number);
            if pending_uploads.get(&token).is_none() {
                pending_uploads.insert(token.clone(), (upload, site.clone()));
                break token;
            }
        };
        core::mem::drop(pending_uploads);

        let token_json = format!("{:?}", token);

        let pool = self.pool().clone();
        let response = JsonFile::with_key_pool(Some(&token_json), pool).unwrap();
        let json_ptr = leak(Box::new(response));

        Ok(ScriptResult::Json(json_ptr))
    }
}


fn leak(json: Box<JsonFile>) -> OpaqueJsonPointer {
    Box::into_raw(json) as _
}

fn get_back(json: OpaqueJsonPointer) -> Box<JsonFile> {
    unsafe { Box::from_raw(json as *mut JsonFile) }
}

static HEX_TO_WORD: [u8; 256] = {
    const __: u8 = 255; // not a hex digit
    [
        //   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 0
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 1
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 2
        00, 01, 02, 03, 04, 05, 06, 07, 08, 09, __, __, __, __, __, __, // 3
        __, 10, 11, 12, 13, 14, 15, __, __, __, __, __, __, __, __, __, // 4
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 5
        __, 10, 11, 12, 13, 14, 15, __, __, __, __, __, __, __, __, __, // 6
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 7
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 8
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // 9
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // A
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // B
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // C
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // D
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // E
        __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, __, // F
    ]
};

pub(crate) fn decode_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() == (N * 2) {
        let mut ret = [0; N];
        let mut iter = hex.as_bytes().iter();

        for i in 0..N {
            let hw = HEX_TO_WORD[*iter.next().unwrap() as usize];
            let lw = HEX_TO_WORD[*iter.next().unwrap() as usize];
            if hw == 255 || lw == 255 {
                return None;
            }

            ret[i] = (hw << 4) | lw;
        }

        Some(ret)
    } else {
        None
    }
}
