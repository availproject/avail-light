use core::convert::TryFrom;

/// Collection of WASM virtual machines.
///
/// The `TUser` generic parameter represents a user data.
pub struct WasmVirtualMachines<TUser> {
    // TODO: remove
    marker: core::marker::PhantomData<TUser>
}

/// WASM blob known to be valid.
pub struct WasmBlob {
    inner: wasmi::Module,
}

/// Identifier for a virtual machine within the collection.
pub struct WasmVmId(u64);

/// One virtual machine within the collection.
pub struct Entry<'a, TUser> {
    vms: &'a WasmVirtualMachines<TUser>,
}

impl<TUser> Default for WasmVirtualMachines<TUser> {
    fn default() -> Self {
        WasmVirtualMachines {
            marker: core::marker::PhantomData,
        }
    }
}

impl<TUser> WasmVirtualMachines<TUser> {
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
}

impl<'a, TUser> Entry<'a, TUser> {
    pub async fn wait(&self) {
        unimplemented!()
    }
}

impl WasmBlob {
    // TODO: better error type
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self, ()> {
        let inner = wasmi::Module::from_buffer(bytes.as_ref()).map_err(|_| ())?;
        inner.deny_floating_point().map_err(|_| ())?;
        Ok(WasmBlob {
            inner,
        })
    }
}

impl<'a> TryFrom<&'a [u8]> for WasmBlob {
    type Error = ();    // TODO: better error type

    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        WasmBlob::from_bytes(bytes)
    }
}
