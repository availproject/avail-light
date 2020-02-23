use core::{future::Future, pin::Pin};

mod vm;

pub use vm::WasmBlob;

/// Collection of WASM virtual machines.
///
/// The `TUser` generic parameter represents a user data.
pub struct WasmVirtualMachines<TUser> {
    /// How to spawn background tasks.
    tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    virtual_machines: Vec<VmState<TUser>>,

    // TODO: remove
    marker: core::marker::PhantomData<TUser>,
}

struct VmState<TUser> {
    /// The inner virtual machine.
    vm: vm::VirtualMachine,
    /// User data decided by the user.
    user_data: TUser,
}

/// Identifier for a virtual machine within the collection.
pub struct WasmVmId(u64);

/// One virtual machine within the collection.
pub struct Entry<'a, TUser> {
    vms: &'a WasmVirtualMachines<TUser>,
}

impl<TUser> WasmVirtualMachines<TUser> {
    /// Initializes the collection.
    pub fn with_executor(
        tasks_executor: impl Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + 'static,
    ) -> Self {
        WasmVirtualMachines {
            tasks_executor: Box::new(tasks_executor),
            virtual_machines: Vec::new(),
            marker: core::marker::PhantomData,
        }
    }

    /// Starts a new virtual machine.
    pub fn start_virtual_machine(&self, user_data: TUser, blob: &WasmBlob) -> Entry<TUser> {
        /*let not_started =
            wasmi::ModuleInstance::new(module.as_ref(), &ImportResolve(RefCell::new(&mut symbols)))
                .map_err(NewErr::Interpreter)?;
        let module = not_started.assert_no_start();

        let indirect_table = if let Some(tbl) = module.export_by_name("__indirect_function_table") {
            if let Some(tbl) = tbl.as_table() {
                Some(tbl.clone())
            } else {
                return Err(NewErr::IndirectTableIsntTable);
            }
        } else {
            None
        };*/

        unimplemented!()
    }

    pub fn get_by_id(&self) -> Option<Entry<TUser>> {
        unimplemented!()
    }

    pub async fn next_event() {}
}

impl<'a, TUser> Entry<'a, TUser> {
    pub async fn wait(&self) {
        unimplemented!()
    }
}
