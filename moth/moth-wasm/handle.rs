use wasmi::{TypedFunc, Memory, AsContext, core::Trap};
use rustgit::{Repository, FileType};
use super::{Pool, wasm::Caller};
use std::sync::{Arc, RwLock};
use core::mem::replace;
use super::PoolStr;
use lmfu::LiteMap;

type Store<'a> = wasmi::StoreContext<'a, Handle>;

pub enum RepositoryHandle {
    None,
    ReadOnly(Arc<RwLock<Repository>>),
    ReadWrite(Arc<RwLock<Repository>>),
}

pub struct Handle {
    pub pool: Pool,
    repo: RepositoryHandle,
    pub token: u64,
    template: Option<PoolStr>,
    parameters: LiteMap<PoolStr, String>,
    db_path: String,

    pub parse_json: Option<TypedFunc<(u64, u64), (u64,)>>,
    pub malloc: Option<TypedFunc<(u64,), (u64,)>>,
    pub free: Option<TypedFunc<(u64, u64), ()>>,
    pub mem: Option<Memory>,
}

pub type TemplateParams = (PoolStr, LiteMap<PoolStr, String>);

impl Handle {
    pub fn new() -> Self {
        Self {
            pool: Pool::get_static_pool(),
            repo: RepositoryHandle::None,
            token: u64::MAX,
            template: None,
            parameters: LiteMap::new(),
            db_path: String::new(),
            parse_json: None,
            malloc: None,
            free: None,
            mem: None,
        }
    }

    pub fn repo(&self, will_write: bool) -> Result<Arc<RwLock<Repository>>, Trap> {
        match (&self.repo, will_write) {
            (RepositoryHandle::None, _) => Err(Trap::new("Nested internal call")),
            (RepositoryHandle::ReadOnly (_  ),  true) => Err(Trap::new("RW/RO barrier")),
            (RepositoryHandle::ReadWrite(arc), false) => Ok(arc.clone()),
            (RepositoryHandle::ReadOnly (arc), false) => Ok(arc.clone()),
            (RepositoryHandle::ReadWrite(arc),  true) => Ok(arc.clone()),
        }
    }

    pub fn init(
        &mut self,
        parse_json: TypedFunc<(u64, u64), (u64,)>,
        malloc: TypedFunc<(u64,), (u64,)>,
        free: TypedFunc<(u64, u64), ()>,
        mem: Memory,
        pool: Pool,
    ) {
        self.parse_json = Some(parse_json);
        self.malloc = Some(malloc);
        self.free = Some(free);
        self.mem = Some(mem);
        self.pool = pool;
    }

    pub fn read_mem<'a>(&self, store: &'a Store, ptr: usize, len: usize) -> Result<&'a [u8], Trap> {
        let fail = || Trap::new("Invalid Pointer");
        let range = ptr..(ptr + len);
        let mem = self.mem.unwrap();
        mem.data(store).get(range).ok_or_else(fail)
    }

    pub fn read_mem_str<'a>(&self, store: &'a Store, ptr: usize, len: usize) -> Result<&'a str, Trap> {
        let fail = || Trap::new("Invalid Bytes");
        let slice = self.read_mem(store, ptr, len)?;
        core::str::from_utf8(slice).ok().ok_or_else(fail)
    }

    pub fn db_path(&mut self, store: Store, tn_len: usize, tn_ptr: usize, key_len: usize, key_ptr: usize) -> Result<&str, Trap> {
        let path_len = tn_len + 1 + key_len + 5;
        self.db_path.clear();
        self.db_path.reserve(path_len);

        self.db_path.push_str(self.read_mem_str(&store, tn_ptr, tn_len)?);
        self.db_path.push_str("/");
        self.db_path.push_str(self.read_mem_str(&store, key_ptr, key_len)?);
        self.db_path.push_str(".json");

        Ok(&*self.db_path)
    }

    pub fn prepare(&mut self, read_only: bool, repo: Arc<RwLock<Repository>>, token: u64) {
        self.token = token;
        self.repo = match read_only {
            true  => RepositoryHandle::ReadOnly (repo),
            false => RepositoryHandle::ReadWrite(repo),
        };
    }

    pub fn reset(&mut self) -> Option<TemplateParams> {
        let this = replace(self, Self::new());
        this.template.map(|t| (t, this.parameters))
    }
}

pub fn read_table_entry(
    mut caller: Caller,
    _db_token: u64,
    tl: u64, // table name
    tp: u64,
    kl: u64, // key
    kp: u64,
    _out_json_len_ptr: u64,
) -> /* out_json_ptr */ Result<u64, Trap> {
    let mut handle = replace(caller.data_mut(), Handle::new());
    let repo = handle.repo(false)?;
    let repo = repo.read().unwrap();

    let file_path = handle.db_path(caller.as_context(), tl as _, tp as _, kl as _, kp as _)?;
    match repo.read_file(file_path) {
        Ok(slice) => {
            let len = slice.len() as u64;

            let ptr = handle.malloc.unwrap().call(&mut caller, (len,))?.0;
            handle.mem.unwrap().write(&mut caller, ptr as _, slice).unwrap();

            let json_ptr = handle.parse_json.unwrap().call(&mut caller, (ptr, len))?.0;
            handle.free.unwrap().call(&mut caller, (ptr, len))?;

            let _ = replace(caller.data_mut(), handle);

            Ok(json_ptr)
        },
        Err(rustgit::Error::PathError) => {
            let _ = replace(caller.data_mut(), handle);
            Ok(0)
        },
        Err(e) => Err(Trap::new(format!("read_table_entry: {:?}", e))),
    }
}

pub fn write_table_entry(
    mut caller: Caller,
    _db_token: u64,
    tl: u64, // table name
    tp: u64,
    kl: u64, // key
    kp: u64,
    json_len: u64,
    json_ptr: u64,
) -> Result<(), Trap> {
    let mut handle = replace(caller.data_mut(), Handle::new());
    let repo = handle.repo(true)?;
    let mut repo = repo.write().unwrap();

    let (jp, jl) = (json_ptr as usize, json_len as usize);
    let bytes = handle.read_mem(&caller.as_context(), jp, jl)?.to_vec();

    let fail = |e| Trap::new(format!("Repository::stage(): {:?}", e));
    let file_path = handle.db_path(caller.as_context(), tl as _, tp as _, kl as _, kp as _)?;
    repo.stage(file_path, Some((bytes, FileType::RegularFile))).map_err(fail)?;

    let _ = replace(caller.data_mut(), handle);
    Ok(())
}

pub fn set_template_name(
    mut caller: Caller,
    _db_token: u64,
    name_len: u64,
    name_ptr: u64,
) -> Result<(), Trap> {
    let mut handle = replace(caller.data_mut(), Handle::new());

    let (np, nl) = (name_ptr as usize, name_len as usize);
    let ctx = caller.as_context();
    let template = handle.read_mem_str(&ctx, np, nl)?;

    handle.template = Some(handle.pool.intern(template));

    let _ = replace(caller.data_mut(), handle);
    Ok(())
}

pub fn set_template_param(
    mut caller: Caller,
    _db_token: u64,
    key_len: u64,
    key_ptr: u64,
    value_len: u64,
    value_ptr: u64,
) -> Result<(), Trap> {
    let mut handle = replace(caller.data_mut(), Handle::new());
    let ctx = caller.as_context();

    let (kp, kl) = (key_ptr as usize, key_len as usize);
    let key = handle.read_mem_str(&ctx, kp, kl)?;
    let key = handle.pool.intern(key);

    let (vp, vl) = (value_ptr as usize, value_len as usize);
    let value = handle.read_mem_str(&ctx, vp, vl)?.to_string();

    handle.parameters.insert(key, value);

    let _ = replace(caller.data_mut(), handle);
    Ok(())
}

