use crate::util::map::stress_sequential;
use cds::{map::SequentialMap, tree::avl_tree::AVLTree};

#[test]
fn test_avl_tree() {
    let mut avl: AVLTree<i32, i32> = AVLTree::new();

    assert_eq!(avl.insert(&1, 1), Ok(()));
    // assert_eq!(avl.insert(&2, 2), Ok(()));
    // assert_eq!(avl.insert(&3, 3), Ok(()));
}

#[test]
fn stress_avl_tree() {
    stress_sequential::<String, AVLTree<_, _>>(100_000);
}
