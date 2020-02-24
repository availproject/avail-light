use core::{future::Future, pin::Pin};
use fnv::FnvBuildHasher;
use futures::{channel::mpsc, prelude::*};
use hashbrown::HashMap;

mod externals;
mod vm;

pub use vm::WasmBlob;

/// Collection of WASM virtual machines.
///
/// The `TUser` generic parameter represents a user data.
pub struct WasmVirtualMachines<TUser> {
    /// How to spawn background tasks.
    tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    virtual_machines: HashMap<WasmVmId, VmState<TUser>, FnvBuildHasher>,

    next_vm_id: WasmVmId,

    /// Sending side of `events_rx`. Cloned every time we spawn a task.
    events_tx: mpsc::Sender<BackToFront>,

    /// Receiver for events.
    events_rx: mpsc::Receiver<BackToFront>,
}

/// Message sent from a background task running a virtual machine to the front API.
enum BackToFront {
    Finished(WasmVmId, Result<Option<wasmi::RuntimeValue>, ()>),
}

struct VmState<TUser> {
    /// The inner virtual machine.
    vm: externals::ExternalsVm,
    /// User data decided by the user.
    user_data: TUser,
}

/// Identifier for a virtual machine within the collection.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
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
        let (events_tx, events_rx) = mpsc::channel(0);

        WasmVirtualMachines {
            tasks_executor: Box::new(tasks_executor),
            virtual_machines: HashMap::default(),
            next_vm_id: WasmVmId(0),
            events_tx,
            events_rx,
        }
    }

    /// Starts a new virtual machine.
    pub fn execute(&self, user_data: TUser, module: &WasmBlob, function: &str, data: &[u8]) -> Entry<TUser> {
        let mut virtual_machine =
            externals::ExternalsVm::new(module, function, data).unwrap(); // TODO: don't unwrap
        let mut events_tx = self.events_tx.clone();
        let wasm_id = self.next_vm_id;
        //self.next_vm_id.0 = self.next_vm_id.0.checked_add(1).unwrap();
        (self.tasks_executor)(Box::pin(async move {
            loop {
                match virtual_machine.state() {
                    externals::State::ReadyToRun(vm) => { vm.run(); },
                    _ => unimplemented!(),
                }
            }
        }));

        Entry {
            vms: self,
        }
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
