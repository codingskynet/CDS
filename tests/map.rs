pub mod random;
use cds::{linkedlist::LinkedList, map::SequentialMap};
use rand::Rng;
use rand::prelude::SliceRandom;
use rand::prelude::ThreadRng;
use rand::thread_rng;
use random::Random;
use std::collections::BTreeMap;
use std::fmt::Debug;

pub fn stress_sequential<K: Ord + Clone + Random + Debug, M: SequentialMap<K, u64>>(iters: u64) {
    let gen_not_existing_key = |rng: &mut ThreadRng, map: &BTreeMap<K, u64>| {
        let mut key = K::gen(rng);

        while map.contains_key(&key) {
            key = K::gen(rng);
        }

        key
    };

    enum Operation {
        Insert,
        Lookup,
        Remove,
    }

    #[derive(PartialEq)]
    enum OperationType {
        Some, // the operation for existing (key, value) on the map
        None, // the operation for not existing (key, value) on the map
    }

    let ops = [
        Operation::Insert,
        Operation::Lookup,
        Operation::Remove,
    ];

    let types = [OperationType::Some, OperationType::None];

    let mut map = M::new();
    let mut ref_map: BTreeMap<K, u64> = BTreeMap::new();
    let mut rng = thread_rng();

    for i in 1..=iters {
        let t = types.choose(&mut rng).unwrap();
        let ref_map_keys = ref_map.keys().collect::<Vec<&K>>();
        let existing_key = ref_map_keys.choose(&mut rng);

        if existing_key.is_none() || *t == OperationType::None { // run operation with not existing key
            let not_existing_key = gen_not_existing_key(&mut rng, &ref_map);

            match ops.choose(&mut rng).unwrap() {
                Operation::Insert => { // should success
                    let data: u64 = rng.gen();

                    assert_eq!(map.insert(&not_existing_key, data), Ok(()));
                    assert_eq!(ref_map.insert(not_existing_key.clone(), data), None);

                    println!("[{:0>10}] InsertNone: ({:?}, {})", i, not_existing_key, data);
                },
                Operation::Lookup => { // should fail
                    assert_eq!(ref_map.get(&not_existing_key), None);
                    assert_eq!(map.lookup(&not_existing_key), None);

                    println!("[{:0>10}] LookupNone: ({:?}, None)", i, not_existing_key);
                },
                Operation::Remove => { // should fail
                    assert_eq!(ref_map.remove(&not_existing_key), None);
                    assert_eq!(map.remove(&not_existing_key), Err(()));

                    println!("[{:0>10}] DeleteNone: ({:?}, Err)", i, not_existing_key);
                },
            }
        } else { // run operation with existing key
            let existing_key = (*existing_key.unwrap()).clone();

            match ops.choose(&mut rng).unwrap() {
                Operation::Insert => { // should fail
                    let data: u64 = rng.gen();

                    assert_eq!(map.insert(&existing_key, data), Err(data));

                    println!("[{:0>10}] InsertSome: ({:?}, {})", i, existing_key, data);
                },
                Operation::Lookup => { // should success
                    let data = ref_map.get(&existing_key);

                    assert_eq!(map.lookup(&existing_key), data);

                    println!("[{:0>10}] LookupSome: ({:?}, {})", i, existing_key, data.unwrap());
                },
                Operation::Remove => { // should success
                    let data = ref_map.remove(&existing_key);

                    assert_eq!(map.remove(&existing_key).ok(), data);

                    println!("[{:0>10}] DeleteSome: ({:?}, {})", i, existing_key, data.unwrap());
                },
            }
        }
    }
}
