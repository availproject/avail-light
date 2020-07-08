mod interpreter;

use smallvec::SmallVec;

pub use interpreter::*;

/// Low-level Wasm function signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Signature {
    params: SmallVec<[ValueType; 2]>,
    ret_ty: Option<ValueType>,
}

/// Easy way to generate a [`Signature`](crate::primitives::Signature).
///
/// # Example
///
/// ```
/// let _sig: redshirt_core::primitives::Signature = redshirt_core::sig!((I32, I64) -> I32);
/// ```
#[macro_export]
macro_rules! sig {
    (($($p:ident),*)) => {{
        let params = core::iter::empty();
        $(let params = params.chain(core::iter::once($crate::ValueType::$p));)*
        $crate::primitives::Signature::new(params, None)
    }};
    (($($p:ident),*) -> $ret:ident) => {{
        let params = core::iter::empty();
        $(let params = params.chain(core::iter::once($crate::ValueType::$p));)*
        $crate::primitives::Signature::new(params, Some($crate::ValueType::$ret))
    }};
}

impl Signature {
    /// Creates a [`Signature`] from the given parameter types and return type.
    pub fn new(
        params: impl Iterator<Item = ValueType>,
        ret_ty: impl Into<Option<ValueType>>,
    ) -> Signature {
        Signature {
            params: params.collect(),
            ret_ty: ret_ty.into(),
        }
    }

    /// Returns a list of all the types of the parameters.
    pub fn parameters(&self) -> impl ExactSizeIterator<Item = &ValueType> {
        self.params.iter()
    }

    /// Returns the type of the return type of the function. `None` means "void".
    pub fn return_type(&self) -> Option<&ValueType> {
        self.ret_ty.as_ref()
    }
}

impl<'a> From<&'a Signature> for wasmi::Signature {
    fn from(sig: &'a Signature) -> wasmi::Signature {
        wasmi::Signature::new(
            sig.params
                .iter()
                .cloned()
                .map(wasmi::ValueType::from)
                .collect::<Vec<_>>(),
            sig.ret_ty.map(wasmi::ValueType::from),
        )
    }
}

impl From<Signature> for wasmi::Signature {
    fn from(sig: Signature) -> wasmi::Signature {
        wasmi::Signature::from(&sig)
    }
}

impl<'a> From<&'a wasmi::Signature> for Signature {
    fn from(sig: &'a wasmi::Signature) -> Signature {
        Signature::new(
            sig.params().iter().cloned().map(ValueType::from),
            sig.return_type().map(ValueType::from),
        )
    }
}

/*impl<'a> From<&'a wasmtime::FuncType> for Signature {
    fn from(sig: &'a wasmtime::FuncType) -> Signature {
        // TODO: we only support one return type at the moment; what even is multiple
        // return types?
        assert!(sig.results().len() <= 1);

        Signature::new(
            sig.params().iter().cloned().map(ValueType::from),
            sig.results().get(0).cloned().map(ValueType::from),
        )
    }
}*/

impl From<wasmi::Signature> for Signature {
    fn from(sig: wasmi::Signature) -> Signature {
        Signature::from(&sig)
    }
}

/// Value that a Wasm function can accept or produce.
#[derive(Debug, Copy, Clone)]
pub enum WasmValue {
    /// A 32-bits integer. There is no fundamental difference between signed and unsigned
    /// integer, and the signed-ness should be determined depending on the context.
    I32(i32),
    /// A 32-bits integer. There is no fundamental difference between signed and unsigned
    /// integer, and the signed-ness should be determined depending on the context.
    I64(i64),
}

/// Type of a value passed as parameter or returned by a function.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ValueType {
    /// A 32-bits integer. Used for both signed and unsigned integers.
    I32,
    /// A 64-bits integer. Used for both signed and unsigned integers.
    I64,
}

impl WasmValue {
    /// Returns the type corresponding to this value.
    pub fn ty(&self) -> ValueType {
        match self {
            WasmValue::I32(_) => ValueType::I32,
            WasmValue::I64(_) => ValueType::I64,
        }
    }

    /// Unwraps [`WasmValue::I32`] into its value.
    pub fn into_i32(self) -> Option<i32> {
        if let WasmValue::I32(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Unwraps [`WasmValue::I64`] into its value.
    pub fn into_i64(self) -> Option<i64> {
        if let WasmValue::I64(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

impl From<wasmi::RuntimeValue> for WasmValue {
    fn from(val: wasmi::RuntimeValue) -> Self {
        match val {
            wasmi::RuntimeValue::I32(v) => WasmValue::I32(v),
            wasmi::RuntimeValue::I64(v) => WasmValue::I64(v),
            _ => panic!(), // TODO: do something other than panicking here
        }
    }
}

impl From<WasmValue> for wasmi::RuntimeValue {
    fn from(val: WasmValue) -> Self {
        match val {
            WasmValue::I32(v) => wasmi::RuntimeValue::I32(v),
            WasmValue::I64(v) => wasmi::RuntimeValue::I64(v),
        }
    }
}

/*impl From<WasmValue> for wasmtime::Val {
    fn from(val: WasmValue) -> Self {
        match val {
            WasmValue::I32(v) => wasmtime::Val::I32(v),
            WasmValue::I64(v) => wasmtime::Val::I64(v),
            _ => unimplemented!(),
        }
    }
}

impl From<wasmtime::Val> for WasmValue {
    fn from(val: wasmtime::Val) -> Self {
        match val {
            wasmtime::Val::I32(v) => WasmValue::I32(v),
            wasmtime::Val::I64(v) => WasmValue::I64(v),
            _ => unimplemented!(),
        }
    }
}*/

impl From<ValueType> for wasmi::ValueType {
    fn from(ty: ValueType) -> wasmi::ValueType {
        match ty {
            ValueType::I32 => wasmi::ValueType::I32,
            ValueType::I64 => wasmi::ValueType::I64,
        }
    }
}

impl From<wasmi::ValueType> for ValueType {
    fn from(val: wasmi::ValueType) -> Self {
        match val {
            wasmi::ValueType::I32 => ValueType::I32,
            wasmi::ValueType::I64 => ValueType::I64,
            _ => panic!(), // TODO: do something other than panicking here
        }
    }
}

/*impl From<wasmtime::ValType> for ValueType {
    fn from(val: wasmtime::ValType) -> Self {
        match val {
            wasmtime::ValType::I32 => ValueType::I32,
            wasmtime::ValType::I64 => ValueType::I64,
            wasmtime::ValType::F32 => ValueType::F32,
            wasmtime::ValType::F64 => ValueType::F64,
            _ => unimplemented!(), // TODO:
        }
    }
}*/
