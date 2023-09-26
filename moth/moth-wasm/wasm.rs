use wasmi::{Engine, Module, Instance, Func, TypedFunc, Value, Memory, core::Trap};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use super::{Pool, Handle, TemplateParams};
use moth::OpaqueJsonPointer;
use rustgit::Repository;
use lmfu::ArrayVec;

pub(crate) type Caller<'a> = wasmi::Caller<'a, Handle>;
type Linker = wasmi::Linker<Handle>;
type Store = wasmi::Store<Handle>;

pub enum RepoBorrow<'a> {
    ReadOnly(RwLockReadGuard<'a, Arc<RwLock<Repository>>>),
    ReadWrite(RwLockWriteGuard<'a, Arc<RwLock<Repository>>>),
}

impl<'a> RepoBorrow<'a> {
    fn repo_arc(&self) -> Arc<RwLock<Repository>> {
        match self {
            Self::ReadOnly(guard) => (*guard).clone(),
            Self::ReadWrite(guard) => (*guard).clone(),
        }
    }
}

pub struct WasmThread {
    module: Arc<Module>,
    instance: Arc<Instance>,
    store: Store,

    parse_json: TypedFunc<(u64, u64), (u64,)>,
    dump_json: TypedFunc<(u64,), (u64,)>,
    json_dump_len: TypedFunc<(u64,), (u64,)>,
    json_dump_ptr: TypedFunc<(u64,), (u64,)>,
    free_json_dump: TypedFunc<(u64,), ()>,
    malloc: TypedFunc<(u64,), (u64,)>,
    free: TypedFunc<(u64, u64), ()>,
    mem: Memory,
}

impl WasmThread {
    fn from_module(module: Arc<Module>, pool: Pool) -> Option<Self> {
        let mut linker: Linker = Linker::new(module.engine());
        let mut store = Store::new(module.engine(), Handle::new());

        let read_table_entry_fn = Func::wrap(&mut store, super::handle::read_table_entry);
        linker.define("host", "read_table_entry", read_table_entry_fn).ok()?;

        let write_table_entry_fn = Func::wrap(&mut store, super::handle::write_table_entry);
        linker.define("host", "write_table_entry", write_table_entry_fn).ok()?;

        let set_template_name_fn = Func::wrap(&mut store, super::handle::set_template_name);
        linker.define("host", "set_template_name", set_template_name_fn).ok()?;

        let set_template_param_fn = Func::wrap(&mut store, super::handle::set_template_param);
        linker.define("host", "set_template_param", set_template_param_fn).ok()?;

        let instance = linker
            .instantiate(&mut store, &module).ok()?
            .start(&mut store).ok()?;

        let malloc = instance.get_typed_func::<(u64,), (u64,)>(&store, "__rs_malloc").ok()?;
        let free = instance.get_typed_func::<(u64, u64), ()>(&store, "__rs_free").ok()?;
        let parse_json = instance.get_typed_func::<(u64, u64), (u64,)>(&store, "__parse_json").ok()?;
        let dump_json = instance.get_typed_func::<(u64,), (u64,)>(&store, "__dump_json").ok()?;
        let json_dump_len = instance.get_typed_func::<(u64,), (u64,)>(&store, "__json_dump_len").ok()?;
        let json_dump_ptr = instance.get_typed_func::<(u64,), (u64,)>(&store, "__json_dump_ptr").ok()?;
        let free_json_dump = instance.get_typed_func::<(u64,), ()>(&store, "__free_json_dump").ok()?;
        let mem = instance.get_memory(&store, "memory")?;

        store.data_mut().init(parse_json, malloc, free, mem, pool);

        Some(Self {
            module,
            instance: Arc::new(instance),
            store,
            malloc,
            free,
            parse_json,
            dump_json,
            json_dump_len,
            json_dump_ptr,
            free_json_dump,
            mem,
        })
    }

    pub fn new(bytes: &[u8], pool: Pool) -> Option<Self> {
        let module = Module::new(&Engine::default(), bytes).unwrap();
        Self::from_module(Arc::new(module), pool)
    }

    fn malloc(&mut self, size: usize) -> Result<u64, Trap> {
        Ok(self.malloc.call(&mut self.store, (size as _,))?.0)
    }

    fn free(&mut self, ptr: u64, size: usize) -> Result<(), Trap> {
        self.free.call(&mut self.store, (ptr, size as _,))
    }

    fn write_mem(&mut self, ptr: u64, buf: &[u8]) -> Result<(), Trap> {
        self.mem.write(&mut self.store, ptr as _, buf).map_err(|e| Trap::new(format!("{:?}", e)))
    }

    pub fn parse_json(&mut self, json: &str) -> Result<OpaqueJsonPointer, Trap> {
        let len = json.len();
        let str_ptr = self.malloc(len)?;
        self.write_mem(str_ptr, json.as_bytes())?;
        let json_ptr = self.parse_json.call(&mut self.store, (str_ptr, len as _,))?.0;
        self.free(str_ptr, len)?;
        Ok(json_ptr as _)
    }

    pub fn dump_json(&mut self, json: OpaqueJsonPointer) -> Result<String, Trap> {
        let arcstr_ptr = self.dump_json.call(&mut self.store, (json as _,))?.0;
        let ptr = self.json_dump_ptr.call(&mut self.store, (arcstr_ptr,))?.0;
        let len = self.json_dump_len.call(&mut self.store, (arcstr_ptr,))?.0;
        let (ptr, len) = (ptr as usize, len as usize);

        let fail = || Trap::new("Invalid Pointer");
        let range = ptr..(ptr + len);
        let slice = self.mem.data(&self.store).get(range).ok_or_else(fail)?;

        let fail = || Trap::new("Invalid Bytes");
        let dump = core::str::from_utf8(slice).ok().ok_or_else(fail)?.to_string();

        self.free_json_dump.call(&mut self.store, (arcstr_ptr,))?;

        Ok(dump)
    }

    pub fn call_script_fn(
        &mut self,
        fn_name: &str,
        read_only: bool,
        repo: &RwLock<Arc<RwLock<Repository>>>,
        db_token: u64,
        req_body: OpaqueJsonPointer,
        req_params: &[String],
    ) -> Result<(Option<TemplateParams>, Option<OpaqueJsonPointer>), Trap> {
        // max: 7 parameters (exc. the id+body pair)
        let mut inputs: ArrayVec<Value, 16> = ArrayVec::new();

        inputs.push(Value::I64(db_token as _));
        inputs.push(Value::I64(req_body as _));

        let len_sum = req_params.iter().fold(0, |a, s| a + s.len());
        let params = self.malloc(len_sum)?;

        let mut ptr = params;
        for string in req_params {
            self.write_mem(ptr, string.as_bytes())?;
            let len = string.len() as u64;
            inputs.push(Value::I64(ptr as _));
            inputs.push(Value::I64(len as _));
            ptr += len;
        }

        let mut outputs = [Value::I64(0)];
        let fail = || Trap::new(format!("Missing callback: {}", fn_name));
        let func = self.instance.get_func(&self.store, fn_name).ok_or_else(fail)?;

        let repo_borrow = match read_only {
            true  => RepoBorrow::ReadOnly (repo. read().unwrap()),
            false => RepoBorrow::ReadWrite(repo.write().unwrap()),
        };

        self.store.data_mut().prepare(read_only, repo_borrow.repo_arc(), db_token);
        match func.call(&mut self.store, &inputs, &mut outputs) {
            Ok(()) => (),
            Err(wasmi::Error::Trap(trap)) => return Err(trap),
            Err(e) => return Err(Trap::new(format!("Wasmi error: {:?}", e))),
        }
        let template = self.store.data_mut().reset();

        core::mem::drop(repo_borrow);
        self.free(params, len_sum)?;

        let fail = || Trap::new("Wrong fn signature");
        let json = match outputs[0].i64().ok_or_else(fail)? {
            0 => None,
            json_ptr => Some(json_ptr as _),
        };

        Ok((template, json))
    }
}

impl Clone for WasmThread {
    fn clone(&self) -> Self {
        Self::from_module(self.module.clone(), self.store.data().pool.clone())
            .unwrap(/* if it worked once, it should work twice */)
    }
}
