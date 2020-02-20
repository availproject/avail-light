/// Collection of WASM virtual machines.
pub struct WasmVirtualMachines {}

/// Identifier for a virtual machine within the collection.
pub struct WasmVmId(u64);

/// One virtual machine within the collection.
pub struct Entry<'a> {
    vms: &'a WasmVirtualMachines,
}

impl Default for WasmVirtualMachines {
    fn default() -> Self {
        WasmVirtualMachines {}
    }
}

impl WasmVirtualMachines {
    /// Starts a new virtual machine.
    pub fn start_virtual_machine(&self, code: &str) -> Entry {
        unimplemented!()
    }

    pub fn get_by_id(&self) -> Option<Entry> {
        unimplemented!()
    }
}

impl<'a> Entry<'a> {
    pub async fn wait(&self) {
        unimplemented!()
    }
}
