/*
 * iterator types
 */

use super::{int, PyInt, PyTypeRef};
use crate::{
    function::ArgCallable,
    protocol::PyIterReturn,
    slots::{IteratorIterable, SlotIterator},
    ItemProtocol, PyClassImpl, PyContext, PyObjectRef, PyRef, PyResult, PyValue, TypeProtocol,
    VirtualMachine,
};
use rustpython_common::{
    lock::{PyMutex, PyRwLock, PyRwLockUpgradableReadGuard},
    static_cell,
};

/// Marks status of iterator.
#[derive(Debug, Clone)]
pub enum IterStatus<T> {
    /// Iterator hasn't raised StopIteration.
    Active(T),
    /// Iterator has raised StopIteration.
    Exhausted,
}

#[derive(Debug)]
pub struct PositionIterInternal<T> {
    pub status: IterStatus<T>,
    pub position: usize,
}

impl<T> PositionIterInternal<T> {
    pub fn new(obj: T, position: usize) -> Self {
        Self {
            status: IterStatus::Active(obj),
            position,
        }
    }

    pub fn set_state(&mut self, state: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        if let IterStatus::Active(_) = &self.status {
            if let Some(i) = state.payload::<PyInt>() {
                let i = int::try_to_primitive(i.as_bigint(), vm).unwrap_or(0);
                self.position = i;
                Ok(())
            } else {
                Err(vm.new_type_error("an integer is required.".to_owned()))
            }
        } else {
            Ok(())
        }
    }

    pub fn set_state_saturated<F>(
        &mut self,
        state: PyObjectRef,
        f: F,
        vm: &VirtualMachine,
    ) -> PyResult<()>
    where
        F: FnOnce(&T) -> usize,
    {
        if let IterStatus::Active(obj) = &self.status {
            if let Some(i) = state.payload::<PyInt>() {
                let i = int::try_to_primitive(i.as_bigint(), vm).unwrap_or(0);
                self.position = i.min(f(obj));
                Ok(())
            } else {
                Err(vm.new_type_error("an integer is required.".to_owned()))
            }
        } else {
            Ok(())
        }
    }

    fn _reduce<F>(&self, func: PyObjectRef, f: F, vm: &VirtualMachine) -> PyObjectRef
    where
        F: FnOnce(&T) -> PyObjectRef,
    {
        if let IterStatus::Active(obj) = &self.status {
            vm.ctx.new_tuple(vec![
                func,
                vm.ctx.new_tuple(vec![f(obj)]),
                vm.ctx.new_int(self.position),
            ])
        } else {
            vm.ctx
                .new_tuple(vec![func, vm.ctx.new_tuple(vec![vm.ctx.new_list(vec![])])])
        }
    }

    pub fn builtin_iter_reduce<F>(&self, f: F, vm: &VirtualMachine) -> PyObjectRef
    where
        F: FnOnce(&T) -> PyObjectRef,
    {
        let iter = builtins_iter(vm).clone();
        self._reduce(iter, f, vm)
    }

    pub fn builtin_reversed_reduce<F>(&self, f: F, vm: &VirtualMachine) -> PyObjectRef
    where
        F: FnOnce(&T) -> PyObjectRef,
    {
        let reversed = builtins_reversed(vm).clone();
        self._reduce(reversed, f, vm)
    }

    pub fn next<F>(&mut self, f: F, vm: &VirtualMachine) -> PyResult
    where
        F: FnOnce(&T, usize) -> PyResult,
    {
        if let IterStatus::Active(obj) = &self.status {
            match f(obj, self.position) {
                Err(e) if e.isinstance(&vm.ctx.exceptions.stop_iteration) => {
                    self.status = IterStatus::Exhausted;
                    Err(e)
                }
                Err(e) if e.isinstance(&vm.ctx.exceptions.index_error) => {
                    self.status = IterStatus::Exhausted;
                    Err(vm.new_stop_iteration())
                }
                Err(e) if e.isinstance(&vm.ctx.exceptions.runtime_error) => {
                    self.status = IterStatus::Exhausted;
                    Err(e)
                }
                Err(e) => Err(e),
                Ok(ret) => {
                    self.position += 1;
                    Ok(ret)
                }
            }
        } else {
            Err(vm.new_stop_iteration())
        }
    }

    pub fn rev_next<F>(&mut self, f: F, vm: &VirtualMachine) -> PyResult
    where
        F: FnOnce(&T, usize) -> PyResult,
    {
        if let IterStatus::Active(obj) = &self.status {
            match f(obj, self.position) {
                Err(e) if e.isinstance(&vm.ctx.exceptions.stop_iteration) => {
                    self.status = IterStatus::Exhausted;
                    Err(e)
                }
                Err(e) if e.isinstance(&vm.ctx.exceptions.index_error) => {
                    self.status = IterStatus::Exhausted;
                    Err(vm.new_stop_iteration())
                }
                Err(e) if e.isinstance(&vm.ctx.exceptions.runtime_error) => {
                    self.status = IterStatus::Exhausted;
                    Err(e)
                }
                Err(e) => Err(e),
                Ok(ret) => {
                    if self.position == 0 {
                        self.status = IterStatus::Exhausted;
                    } else {
                        self.position -= 1;
                    }
                    Ok(ret)
                }
            }
        } else {
            Err(vm.new_stop_iteration())
        }
    }

    pub fn length_hint<F>(&self, f: F) -> usize
    where
        F: FnOnce(&T) -> usize,
    {
        if let IterStatus::Active(obj) = &self.status {
            f(obj).saturating_sub(self.position)
        } else {
            0
        }
    }

    pub fn rev_length_hint<F>(&self, f: F) -> usize
    where
        F: FnOnce(&T) -> usize,
    {
        if let IterStatus::Active(obj) = &self.status {
            if self.position <= f(obj) {
                return self.position + 1;
            }
        }
        0
    }
}

pub fn builtins_iter(vm: &VirtualMachine) -> &PyObjectRef {
    static_cell! {
        static INSTANCE: PyObjectRef;
    }
    INSTANCE.get_or_init(|| vm.get_attribute(vm.builtins.clone(), "iter").unwrap())
}

pub fn builtins_reversed(vm: &VirtualMachine) -> &PyObjectRef {
    static_cell! {
        static INSTANCE: PyObjectRef;
    }
    INSTANCE.get_or_init(|| vm.get_attribute(vm.builtins.clone(), "reversed").unwrap())
}

#[pyclass(module = false, name = "iterator")]
#[derive(Debug)]
pub struct PySequenceIterator {
    internal: PyMutex<PositionIterInternal<PyObjectRef>>,
}

impl PyValue for PySequenceIterator {
    fn class(vm: &VirtualMachine) -> &PyTypeRef {
        &vm.ctx.types.iter_type
    }
}

#[pyimpl(with(SlotIterator))]
impl PySequenceIterator {
    pub fn new(obj: PyObjectRef) -> Self {
        Self {
            internal: PyMutex::new(PositionIterInternal::new(obj, 0)),
        }
    }

    #[pymethod(magic)]
    fn length_hint(&self, vm: &VirtualMachine) -> PyObjectRef {
        let internal = self.internal.lock();
        if let IterStatus::Active(obj) = &internal.status {
            vm.obj_len(obj)
                .map(|x| PyInt::from(x).into_object(vm))
                .unwrap_or_else(|_| vm.ctx.not_implemented())
        } else {
            PyInt::from(0).into_object(vm)
        }
    }

    #[pymethod(magic)]
    fn reduce(&self, vm: &VirtualMachine) -> PyObjectRef {
        self.internal.lock().builtin_iter_reduce(|x| x.clone(), vm)
    }

    #[pymethod(magic)]
    fn setstate(&self, state: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        self.internal.lock().set_state(state, vm)
    }
}

impl IteratorIterable for PySequenceIterator {}
impl SlotIterator for PySequenceIterator {
    fn next(zelf: &PyRef<Self>, vm: &VirtualMachine) -> PyResult {
        zelf.internal
            .lock()
            .next(|obj, pos| obj.get_item(pos, vm), vm)
    }
}

#[pyclass(module = false, name = "callable_iterator")]
#[derive(Debug)]
pub struct PyCallableIterator {
    sentinel: PyObjectRef,
    status: PyRwLock<IterStatus<ArgCallable>>,
}

impl PyValue for PyCallableIterator {
    fn class(vm: &VirtualMachine) -> &PyTypeRef {
        &vm.ctx.types.callable_iterator
    }
}

#[pyimpl(with(SlotIterator))]
impl PyCallableIterator {
    pub fn new(callable: ArgCallable, sentinel: PyObjectRef) -> Self {
        Self {
            sentinel,
            status: PyRwLock::new(IterStatus::Active(callable)),
        }
    }
}

impl IteratorIterable for PyCallableIterator {}
impl SlotIterator for PyCallableIterator {
    fn next(zelf: &PyRef<Self>, vm: &VirtualMachine) -> PyResult {
        let status = zelf.status.upgradable_read();
        if let IterStatus::Active(callable) = &*status {
            let ret = callable.invoke((), vm)?;
            if vm.bool_eq(&ret, &zelf.sentinel)? {
                *PyRwLockUpgradableReadGuard::upgrade(status) = IterStatus::Exhausted;
                Err(vm.new_stop_iteration())
            } else {
                Ok(ret)
            }
        } else {
            Err(vm.new_stop_iteration())
        }
    }
}

pub fn init(context: &PyContext) {
    PySequenceIterator::extend_class(context, &context.types.iter_type);
    PyCallableIterator::extend_class(context, &context.types.callable_iterator);
}
