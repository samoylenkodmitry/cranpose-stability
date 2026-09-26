//! Check the linter's comparison model against the real pinned Cranpose runtime.
use cranpose_core::ParamState;
use std::{cell::RefCell, rc::Rc};

#[test]
fn equal_collections_skip_parameter_changes() {
    let mut slot = ParamState::<Vec<String>>::default();
    let value = vec!["same".to_string()];
    assert!(slot.update(&value));
    assert!(!slot.update(&value.clone()));
}
#[test]
fn cloned_shared_mutable_value_can_hide_a_change() {
    let mut slot = ParamState::<Rc<RefCell<i32>>>::default();
    let value = Rc::new(RefCell::new(1));
    assert!(slot.update(&value));
    *value.borrow_mut() = 2;
    assert!(
        !slot.update(&value),
        "both handles now observe 2; there is no retained old value"
    );
}
#[test]
fn owned_cell_takes_a_value_snapshot() {
    let mut slot = ParamState::<RefCell<i32>>::default();
    let value = RefCell::new(1);
    assert!(slot.update(&value));
    *value.borrow_mut() = 2;
    assert!(slot.update(&value));
}
