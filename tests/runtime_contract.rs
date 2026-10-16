//! Check the linter's comparison model against the real Cranpose runtime from crates.io.
use cranpose_core::{ParamState, refresh_param, refresh_shared_param};
use std::{cell::RefCell, rc::Rc};

#[test]
fn equal_collections_skip_parameter_changes() {
    let mut slot = ParamState::<Vec<String>>::default();
    let value = vec!["same".to_string()];
    let equal = value.clone();
    assert!(slot.update_fields(|| value.clone(), |stored| refresh_param(stored, &value)));
    assert!(!slot.update_fields(|| equal.clone(), |stored| refresh_param(stored, &equal)));
}
#[test]
fn cloned_shared_mutable_value_can_hide_a_change() {
    let mut slot = ParamState::<Rc<RefCell<i32>>>::default();
    let value = Rc::new(RefCell::new(1));
    assert!(slot.update_fields(
        || value.clone(),
        |stored| refresh_shared_param(stored, &value)
    ));
    *value.borrow_mut() = 2;
    assert!(
        !slot.update_fields(
            || value.clone(),
            |stored| refresh_shared_param(stored, &value)
        ),
        "both handles now observe 2; there is no retained old value"
    );
}
#[test]
fn owned_cell_takes_a_value_snapshot() {
    let mut slot = ParamState::<RefCell<i32>>::default();
    let value = RefCell::new(1);
    assert!(slot.update_fields(|| value.clone(), |stored| refresh_param(stored, &value)));
    *value.borrow_mut() = 2;
    assert!(slot.update_fields(|| value.clone(), |stored| refresh_param(stored, &value)));
}
