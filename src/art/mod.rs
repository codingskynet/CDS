use std::{
    cmp::Ordering,
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
};

use arr_macro::arr;
use either::Either;
use std::fmt::Debug;

use crate::{
    left_or,
    map::SequentialMap,
    util::{slice_insert, slice_remove},
};

const PREFIX_LEN: usize = 12;
#[derive(Debug)]
struct NodeHeader {
    len: u32,                 // the len of prefix
    prefix: [u8; PREFIX_LEN], // prefix for path compression
}

impl Default for NodeHeader {
    #[allow(deprecated)]
    fn default() -> Self {
        unsafe {
            Self {
                len: 0,
                prefix: mem::uninitialized(),
            }
        }
    }
}

/// the child node type
/// This is used for bitflag on child pointer.
const NODETYPE_MASK: usize = 0b111;
#[repr(usize)]
enum NodeType {
    Value = 0b000,
    Node4 = 0b001,
    Node16 = 0b010,
    Node48 = 0b011,
    Node256 = 0b100,
}

trait NodeOps<V> {
    fn header(&self) -> &NodeHeader;
    fn header_mut(&mut self) -> &mut NodeHeader;
    fn is_full(&self) -> bool;
    fn is_shrinkable(&self) -> bool;
    fn get_any_child(&self) -> Option<NodeV<V>>;
    fn insert(&mut self, key: u8, node: Node<V>) -> Result<(), Node<V>>;
    fn lookup(&self, key: u8) -> Option<&Node<V>>;
    fn lookup_mut(&mut self, key: u8) -> Option<&mut Node<V>>;
    fn update(&mut self, key: u8, node: Node<V>) -> Result<Node<V>, Node<V>>;
    fn remove(&mut self, key: u8) -> Result<Node<V>, ()>;
}

/// the pointer struct for Nodes or value
struct Node<V> {
    pointer: usize,
    _marker: PhantomData<Box<V>>,
}

impl<V: Debug> Debug for Node<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe {
            let pointer = self.pointer & !NODETYPE_MASK;
            let tag = mem::transmute(self.pointer & NODETYPE_MASK);

            match tag {
                NodeType::Value => (&*(pointer as *const NodeV<V>)).fmt(f),
                NodeType::Node4 => (&*(pointer as *const Node4<V>)).fmt(f),
                NodeType::Node16 => (&*(pointer as *const Node16<V>)).fmt(f),
                NodeType::Node48 => (&*(pointer as *const Node48<V>)).fmt(f),
                NodeType::Node256 => (&*(pointer as *const Node256<V>)).fmt(f),
            }
        }
    }
}

impl<V> Node<V> {
    fn deref(&self) -> Either<&dyn NodeOps<V>, &NodeV<V>> {
        unsafe {
            let pointer = self.pointer & !NODETYPE_MASK;
            let tag = mem::transmute(self.pointer & NODETYPE_MASK);

            match tag {
                NodeType::Value => Either::Right(&*(pointer as *const NodeV<V>)),
                NodeType::Node4 => Either::Left(&*(pointer as *const Node4<V>)),
                NodeType::Node16 => Either::Left(&*(pointer as *const Node16<V>)),
                NodeType::Node48 => Either::Left(&*(pointer as *const Node48<V>)),
                NodeType::Node256 => Either::Left(&*(pointer as *const Node256<V>)),
            }
        }
    }

    fn deref_mut(&self) -> Either<&mut dyn NodeOps<V>, &mut NodeV<V>> {
        unsafe {
            let pointer = self.pointer & !NODETYPE_MASK;
            let tag = mem::transmute(self.pointer & NODETYPE_MASK);

            match tag {
                NodeType::Value => Either::Right(&mut *(pointer as *mut NodeV<V>)),
                NodeType::Node4 => Either::Left(&mut *(pointer as *mut Node4<V>)),
                NodeType::Node16 => Either::Left(&mut *(pointer as *mut Node16<V>)),
                NodeType::Node48 => Either::Left(&mut *(pointer as *mut Node48<V>)),
                NodeType::Node256 => Either::Left(&mut *(pointer as *mut Node256<V>)),
            }
        }
    }

    fn new<T>(node: T, node_type: NodeType) -> Self {
        let node = Box::into_raw(Box::new(node));

        Self {
            pointer: node as usize | node_type as usize,
            _marker: PhantomData,
        }
    }

    const fn null() -> Self {
        Self {
            pointer: 0,
            _marker: PhantomData,
        }
    }

    #[inline]
    fn is_null(&self) -> bool {
        self.pointer == 0
    }

    fn node_type(&self) -> NodeType {
        unsafe { mem::transmute(self.pointer & NODETYPE_MASK) }
    }

    /// extend node to bigger one only if necessary
    fn extend(&mut self) {
        if self.deref().is_right() {
            return;
        }

        if !self.deref().left().unwrap().is_full() {
            return;
        }

        let node_type = self.node_type();
        let node = self.deref_mut().left().unwrap();

        match node_type {
            NodeType::Value => unreachable!(),
            NodeType::Node4 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node4<V>;
                let new = Box::new(Node16::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
            NodeType::Node16 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node16<V>;
                let new = Box::new(Node48::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
            NodeType::Node48 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node48<V>;
                let new = Box::new(Node256::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
            NodeType::Node256 => panic!("Node256 cannot be extended."),
        }
    }

    /// shrink node to smaller one only if necessary
    fn shrink(&mut self) {
        if self.deref().is_right() {
            return;
        }

        if !self.deref().left().unwrap().is_shrinkable() {
            return;
        }

        let node_type = self.node_type();
        let node = self.deref_mut().left().unwrap();

        match node_type {
            NodeType::Value => unreachable!(),
            NodeType::Node4 => panic!("Node4 cannot be shrinked."),
            NodeType::Node16 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node16<V>;
                let new = Box::new(Node4::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
            NodeType::Node48 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node48<V>;
                let new = Box::new(Node16::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
            NodeType::Node256 => unsafe {
                let node = node as *const dyn NodeOps<V> as *const Node256<V>;
                let new = Box::new(Node48::from(ptr::read(node)));
                self.pointer = Box::into_raw(new) as usize | node_type as usize;
            },
        }
    }

    /// compare the keys from depth to header.len
    fn prefix_match(keys: &[u8], node: &dyn NodeOps<V>, depth: usize) -> Result<(), usize> {
        let header = node.header();

        for (index, prefix) in unsafe {
            header
                .prefix
                .get_unchecked(..header.len as usize)
                .iter()
                .enumerate()
        } {
            if keys[depth + index] != *prefix {
                return Err(depth + index);
            }
        }

        if header.len > PREFIX_LEN as u32 {
            // check strictly by using leaf node
            let any_child = node.get_any_child().unwrap();

            let mut depth = depth + PREFIX_LEN;

            while depth < depth + header.len as usize {
                if keys[depth] != any_child.key[depth] {
                    return Err(depth);
                }

                depth += 1;
            }
        }

        Ok(())
    }
}

struct NodeV<V> {
    key: Box<[u8]>,
    value: V,
}

impl<V: Debug> Debug for NodeV<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeV")
            .field("key", &self.key)
            .field("value", &self.value)
            .finish()
    }
}

impl<V> NodeV<V> {
    fn new(key: Vec<u8>, value: V) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

struct Node4<V> {
    header: NodeHeader,
    len: usize,
    keys: [u8; 4],
    children: [Node<V>; 4],
}

impl<V: Debug> Debug for Node4<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node4")
            .field("header", &self.header)
            .field("len", &self.len)
            .field("keys", &self.keys())
            .field("children", &self.children())
            .finish()
    }
}

impl<V> Default for Node4<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        unsafe {
            Self {
                header: Default::default(),
                len: 0,
                keys: mem::uninitialized(),
                children: mem::uninitialized(),
            }
        }
    }
}

impl<V> From<Node16<V>> for Node4<V> {
    fn from(node: Node16<V>) -> Self {
        debug_assert!(node.len <= 4);

        let mut new = Self::default();
        new.header = node.header;
        new.len = node.len;

        unsafe {
            ptr::copy_nonoverlapping(node.keys.as_ptr(), new.keys.as_mut_ptr(), node.len as usize);
            ptr::copy_nonoverlapping(
                node.children.as_ptr(),
                new.children.as_mut_ptr(),
                node.len as usize,
            );
        }

        new
    }
}

impl<V> Node4<V> {
    fn keys(&self) -> &[u8] {
        unsafe { self.keys.get_unchecked(..self.len as usize) }
    }

    fn mut_keys(&mut self) -> &mut [u8] {
        unsafe { self.keys.get_unchecked_mut(..self.len as usize) }
    }

    fn children(&self) -> &[Node<V>] {
        unsafe { self.children.get_unchecked(..self.len as usize) }
    }

    fn mut_children(&mut self) -> &mut [Node<V>] {
        unsafe { self.children.get_unchecked_mut(..self.len as usize) }
    }
}

impl<V> NodeOps<V> for Node4<V> {
    #[inline]
    fn header(&self) -> &NodeHeader {
        &self.header
    }

    #[inline]
    fn header_mut(&mut self) -> &mut NodeHeader {
        &mut self.header
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.len == 4
    }

    #[inline]
    fn is_shrinkable(&self) -> bool {
        false
    }

    fn get_any_child(&self) -> Option<NodeV<V>> {
        todo!()
    }

    fn insert(&mut self, key: u8, node: Node<V>) -> Result<(), Node<V>> {
        debug_assert!(!self.is_full());

        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => unsafe {
                    self.len += 1;
                    slice_insert(self.mut_keys(), index, key);
                    slice_insert(self.mut_children(), index, node);
                    return Ok(());
                },
                Ordering::Equal => return Err(node),
                Ordering::Greater => {}
            }
        }

        Err(node)
    }

    fn lookup(&self, key: u8) -> Option<&Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            if key == *k {
                return unsafe { Some(self.children.get_unchecked(index)) };
            }
        }

        None
    }

    fn lookup_mut(&mut self, key: u8) -> Option<&mut Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            if key == *k {
                return unsafe { Some(self.children.get_unchecked_mut(index)) };
            }
        }

        None
    }

    fn update(&mut self, key: u8, node: Node<V>) -> Result<Node<V>, Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => {}
                Ordering::Equal => unsafe {
                    let node = mem::replace(self.children.get_unchecked_mut(index), node);
                    return Ok(node);
                },
                Ordering::Greater => {}
            }
        }

        Err(node)
    }

    fn remove(&mut self, key: u8) -> Result<Node<V>, ()> {
        debug_assert!(self.len != 0);

        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => {}
                Ordering::Equal => unsafe {
                    self.len -= 1;
                    let node = mem::replace(self.children.get_unchecked_mut(index), Node::null());
                    return Ok(node);
                },
                Ordering::Greater => {}
            }
        }

        Err(())
    }
}

struct Node16<V> {
    header: NodeHeader,
    len: usize,
    keys: [u8; 16],
    children: [Node<V>; 16],
}

impl<V: Debug> Debug for Node16<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node16")
            .field("header", &self.header)
            .field("len", &self.len)
            .field("keys", &self.keys())
            .field("children", &self.children())
            .finish()
    }
}

impl<V> Default for Node16<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        unsafe {
            Self {
                header: Default::default(),
                len: 0,
                keys: mem::uninitialized(),
                children: mem::uninitialized(),
            }
        }
    }
}

impl<V> From<Node4<V>> for Node16<V> {
    fn from(node: Node4<V>) -> Self {
        debug_assert!(node.len == 4);

        let mut new = Self::default();
        new.header = node.header;
        new.len = node.len;

        unsafe {
            ptr::copy_nonoverlapping(node.keys.as_ptr(), new.keys.as_mut_ptr(), node.len as usize);
            ptr::copy_nonoverlapping(
                node.children.as_ptr(),
                new.children.as_mut_ptr(),
                node.len as usize,
            );
        }

        new
    }
}

impl<V> From<Node48<V>> for Node16<V> {
    fn from(node: Node48<V>) -> Self {
        debug_assert!(node.len <= 16);

        let mut new = Self::default();
        new.header = node.header;
        new.len = node.len;

        unsafe {
            let mut i = 0;
            for (key, index) in node.keys.iter().enumerate() {
                if *index != 0xff {
                    *new.keys.get_unchecked_mut(i) = key as u8;
                    *new.children.get_unchecked_mut(i) =
                        ptr::read(node.children.get_unchecked(*index as usize));
                    i += 1;
                }
            }
        }

        new
    }
}

impl<V> Node16<V> {
    fn keys(&self) -> &[u8] {
        unsafe { self.keys.get_unchecked(..self.len as usize) }
    }

    fn mut_keys(&mut self) -> &mut [u8] {
        unsafe { self.keys.get_unchecked_mut(..self.len as usize) }
    }

    fn children(&self) -> &[Node<V>] {
        unsafe { self.children.get_unchecked(..self.len as usize) }
    }

    fn mut_children(&mut self) -> &mut [Node<V>] {
        unsafe { self.children.get_unchecked_mut(..self.len as usize) }
    }
}

impl<V> NodeOps<V> for Node16<V> {
    #[inline]
    fn header(&self) -> &NodeHeader {
        &self.header
    }

    #[inline]
    fn header_mut(&mut self) -> &mut NodeHeader {
        &mut self.header
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.len == 16
    }

    #[inline]
    fn is_shrinkable(&self) -> bool {
        self.len <= 4
    }

    fn get_any_child(&self) -> Option<NodeV<V>> {
        todo!()
    }

    fn insert(&mut self, key: u8, node: Node<V>) -> Result<(), Node<V>> {
        debug_assert!(!self.is_full());

        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => unsafe {
                    self.len += 1;
                    slice_insert(self.mut_keys(), index, key);
                    slice_insert(self.mut_children(), index, node);
                    return Ok(());
                },
                Ordering::Equal => return Err(node),
                Ordering::Greater => {}
            }
        }

        Err(node)
    }

    fn lookup(&self, key: u8) -> Option<&Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            if key == *k {
                return unsafe { Some(self.children.get_unchecked(index)) };
            }
        }

        None
    }

    fn lookup_mut(&mut self, key: u8) -> Option<&mut Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            if key == *k {
                return unsafe { Some(self.children.get_unchecked_mut(index)) };
            }
        }

        None
    }

    fn update(&mut self, key: u8, node: Node<V>) -> Result<Node<V>, Node<V>> {
        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => {}
                Ordering::Equal => unsafe {
                    let node = mem::replace(self.children.get_unchecked_mut(index), node);
                    return Ok(node);
                },
                Ordering::Greater => {}
            }
        }

        Err(node)
    }

    fn remove(&mut self, key: u8) -> Result<Node<V>, ()> {
        debug_assert!(self.len != 0);

        for (index, k) in self.keys().iter().enumerate() {
            match key.cmp(k) {
                Ordering::Less => {}
                Ordering::Equal => unsafe {
                    self.len -= 1;
                    let node = mem::replace(self.children.get_unchecked_mut(index), Node::null());
                    return Ok(node);
                },
                Ordering::Greater => {}
            }
        }

        Err(())
    }
}
struct Node48<V> {
    header: NodeHeader,
    len: usize,
    keys: [u8; 256],
    children: [Node<V>; 48],
}

impl<V: Debug> Debug for Node48<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let valid_keys = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, index)| **index != 0xff)
            .map(|(key, _)| key)
            .collect::<Vec<_>>();

        f.debug_struct("Node48")
            .field("header", &self.header)
            .field("len", &self.len)
            .field("keys", &valid_keys)
            .field("children", &self.children())
            .finish()
    }
}

impl<V> Default for Node48<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        Self {
            header: Default::default(),
            len: 0,
            keys: arr![0xff; 256], // the invalid index is 0xff
            children: arr![Node::null(); 48],
        }
    }
}

impl<V> From<Node16<V>> for Node48<V> {
    fn from(node: Node16<V>) -> Self {
        debug_assert!(node.len == 16);

        let mut new = Self::default();

        unsafe {
            for (index, key) in node.keys().iter().enumerate() {
                *new.keys.get_unchecked_mut(*key as usize) = index as u8;
            }

            ptr::copy_nonoverlapping(
                node.children.as_ptr(),
                new.children.as_mut_ptr(),
                node.len as usize,
            );
        }

        new.header = node.header;
        new.len = node.len;

        new
    }
}

impl<V> From<Node256<V>> for Node48<V> {
    fn from(node: Node256<V>) -> Self {
        debug_assert!(node.len <= 48);

        let mut new = Self::default();

        unsafe {
            // TODO: child is dropping?
            for (key, child) in node.children.iter().enumerate() {
                if !child.is_null() {
                    new.len += 1;
                    *new.keys.get_unchecked_mut(key) = (new.len - 1) as u8;
                    *new.children.get_unchecked_mut(new.len - 1) = ptr::read(child);
                }
            }
        }

        new.header = node.header;
        new.len = node.len;

        new
    }
}

impl<V> Node48<V> {
    fn children(&self) -> &[Node<V>] {
        unsafe { self.children.get_unchecked(..self.len as usize) }
    }

    fn mut_children(&mut self) -> &mut [Node<V>] {
        unsafe { self.children.get_unchecked_mut(..self.len as usize) }
    }
}

impl<V> NodeOps<V> for Node48<V> {
    #[inline]
    fn header(&self) -> &NodeHeader {
        &self.header
    }

    #[inline]
    fn header_mut(&mut self) -> &mut NodeHeader {
        &mut self.header
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.len == 48
    }

    #[inline]
    fn is_shrinkable(&self) -> bool {
        self.len <= 16
    }

    fn get_any_child(&self) -> Option<NodeV<V>> {
        todo!()
    }

    fn insert(&mut self, key: u8, node: Node<V>) -> Result<(), Node<V>> {
        debug_assert!(!self.is_full());

        let index = unsafe { self.keys.get_unchecked_mut(key as usize) };

        if *index != 0xff {
            Err(node)
        } else {
            unsafe {
                *self.children.get_unchecked_mut(self.len) = node;
            }

            *index = self.len as u8;
            self.len += 1;
            Ok(())
        }
    }

    fn lookup(&self, key: u8) -> Option<&Node<V>> {
        let index = unsafe { self.keys.get_unchecked(key as usize) };

        if *index == 0xff {
            None
        } else {
            unsafe { Some(self.children.get_unchecked(*index as usize)) }
        }
    }

    fn lookup_mut(&mut self, key: u8) -> Option<&mut Node<V>> {
        let index = unsafe { self.keys.get_unchecked(key as usize) };

        if *index == 0xff {
            None
        } else {
            unsafe { Some(self.children.get_unchecked_mut(*index as usize)) }
        }
    }

    fn update(&mut self, key: u8, node: Node<V>) -> Result<Node<V>, Node<V>> {
        let index = unsafe { self.keys.get_unchecked_mut(key as usize) };

        if *index == 0xff {
            Err(node)
        } else {
            let child = unsafe { self.children.get_unchecked_mut(*index as usize) };
            let old = mem::replace(child, node);
            Ok(old)
        }
    }

    fn remove(&mut self, key: u8) -> Result<Node<V>, ()> {
        let index = unsafe { self.keys.get_unchecked(key as usize).clone() };

        if index == 0xff {
            Err(())
        } else {
            unsafe {
                let node = slice_remove(self.mut_children(), index as usize);
                *self.keys.get_unchecked_mut(key as usize) = 0xff;
                self.len -= 1;
                Ok(node)
            }
        }
    }
}

struct Node256<V> {
    header: NodeHeader,
    len: usize,
    children: [Node<V>; 256],
}

impl<V: Debug> Debug for Node256<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let valid_children = self
            .children
            .iter()
            .enumerate()
            .filter(|(_, child)| !child.is_null())
            .collect::<Vec<_>>();

        f.debug_struct("Node256")
            .field("header", &self.header)
            .field("len", &self.len)
            .field("children", &valid_children)
            .finish()
    }
}

impl<V> Default for Node256<V> {
    #[allow(deprecated)]
    fn default() -> Self {
        Self {
            header: Default::default(),
            len: 0,
            children: arr![Node::null(); 256],
        }
    }
}

impl<V> From<Node48<V>> for Node256<V> {
    fn from(node: Node48<V>) -> Self {
        debug_assert!(node.len == 48);

        let mut new = Self::default();

        unsafe {
            for (key, index) in node.keys.iter().enumerate() {
                if *index != 0xff {
                    *new.children.get_unchecked_mut(key) =
                        ptr::read(node.children.get_unchecked(*index as usize));
                }
            }
        }

        new.header = node.header;
        new.len = node.len;

        new
    }
}

impl<V> NodeOps<V> for Node256<V> {
    #[inline]
    fn header(&self) -> &NodeHeader {
        &self.header
    }

    #[inline]
    fn header_mut(&mut self) -> &mut NodeHeader {
        &mut self.header
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.len == 256
    }

    #[inline]
    fn is_shrinkable(&self) -> bool {
        self.len <= 48
    }

    fn get_any_child(&self) -> Option<NodeV<V>> {
        todo!()
    }

    fn insert(&mut self, key: u8, node: Node<V>) -> Result<(), Node<V>> {
        let child = unsafe { self.children.get_unchecked_mut(key as usize) };

        if child.is_null() {
            *child = node;
            Ok(())
        } else {
            Err(node)
        }
    }

    fn lookup(&self, key: u8) -> Option<&Node<V>> {
        let child = unsafe { self.children.get_unchecked(key as usize) };

        if child.is_null() {
            None
        } else {
            Some(child)
        }
    }

    fn lookup_mut(&mut self, key: u8) -> Option<&mut Node<V>> {
        let child = unsafe { self.children.get_unchecked_mut(key as usize) };

        if child.is_null() {
            None
        } else {
            Some(child)
        }
    }

    fn update(&mut self, key: u8, node: Node<V>) -> Result<Node<V>, Node<V>> {
        let child = unsafe { self.children.get_unchecked_mut(key as usize) };

        if child.is_null() {
            Err(node)
        } else {
            let old = mem::replace(child, node);
            Ok(old)
        }
    }

    fn remove(&mut self, key: u8) -> Result<Node<V>, ()> {
        let child = unsafe { self.children.get_unchecked_mut(key as usize) };

        if child.is_null() {
            Err(())
        } else {
            let node = mem::replace(child, Node::null());
            Ok(node)
        }
    }
}

pub trait Encodable {
    fn encode(&self) -> Vec<u8>;
}

impl Encodable for String {
    fn encode(&self) -> Vec<u8> {
        self.clone().into_bytes()
    }
}

struct Cursor<V> {
    parent: Option<NonNull<Node<V>>>,
    current: NonNull<Node<V>>,
}

pub struct ART<K, V> {
    root: Node<V>,
    _marker: PhantomData<K>,
}

impl<K, V: Debug> Debug for ART<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ART").field("root", &self.root).finish()
    }
}

impl<K, V> ART<K, V> {}

impl<K: Eq + Encodable, V> SequentialMap<K, V> for ART<K, V> {
    fn new() -> Self {
        let root = Node::new(Node256::<V>::default(), NodeType::Node256);

        Self {
            root,
            _marker: PhantomData,
        }
    }

    fn insert(&mut self, key: &K, value: V) -> Result<(), V> {
        let keys = key.encode();
        let mut depth = 0;
        let mut prefix_len: u32 = 0;
        let mut parent = None;
        let mut current = NonNull::new(&mut self.root).unwrap();

        while depth < keys.len() {
            let current_ref = unsafe { current.as_mut() };
            let node = left_or!(current_ref.deref_mut(), break);

            if let Err(common_depth) = Node::prefix_match(&keys, node, depth) {
                prefix_len = (common_depth - depth) as u32;
                break;
            }

            let prefix = node.header().len;

            if let Some(node) = node.lookup_mut(keys[depth]) {
                depth += 1 + prefix as usize;
                parent = Some(current);
                current = NonNull::new(node).unwrap();
            } else {
                prefix_len = prefix;
                break;
            }
        }

        let current_ref = unsafe { current.as_mut() };
        current_ref.extend();

        match current_ref.deref_mut() {
            Either::Left(node) => {
                let key = keys[depth];
                let new = NodeV::new(keys.clone(), value);

                if prefix_len == node.header().len {
                    // just insert value into this node
                    let insert = node.insert(key, Node::new(new, NodeType::Value));
                    debug_assert!(insert.is_ok());
                } else {
                    // split prefix
                    let mut inter_node = Node4::<V>::default();
                    inter_node
                        .header
                        .prefix
                        .clone_from_slice(&keys[depth..(depth + prefix_len as usize)]);
                    inter_node.header.len = prefix_len;

                    let mut inter_node_ptr = NonNull::new(&mut inter_node).unwrap();

                    // re-set the old's prefix
                    let header = node.header_mut();
                    let prefix = header.prefix.clone();
                    unsafe {
                        ptr::copy_nonoverlapping(
                            prefix.as_ptr(),
                            header.prefix.as_mut_ptr(),
                            (header.len - prefix_len) as usize,
                        )
                    };
                    header.len = header.len - prefix_len;

                    let old = unsafe {
                        mem::replace(current.as_mut(), Node::new(inter_node, NodeType::Node4))
                    };

                    let inter_node_ptr = unsafe { inter_node_ptr.as_mut() };
                    let insert_old = inter_node_ptr
                        .insert(node.header().prefix[depth + prefix_len as usize], old);
                    debug_assert!(insert_old.is_ok());
                    let insert_new = inter_node_ptr.insert(key, Node::new(new, NodeType::Value));
                    debug_assert!(insert_new.is_ok());
                }

                Ok(())
            }
            Either::Right(_) => Err(value),
        }
    }

    fn lookup(&self, key: &K) -> Option<&V> {
        let keys = key.encode();
        let mut depth = 0;

        let mut current = &self.root;

        while depth < keys.len() {
            let node = left_or!(current.deref(), return None);
            depth += node.header().len as usize;

            if let Some(node) = node.lookup(keys[depth]) {
                depth += 1;
                current = node;
            } else {
                return None;
            }
        }

        match current.deref() {
            Either::Left(_) => None,
            Either::Right(nodev) => {
                if *nodev.key == keys {
                    Some(&nodev.value)
                } else {
                    None
                }
            }
        }
    }

    fn remove(&mut self, key: &K) -> Result<V, ()> {
        todo!()
    }
}
