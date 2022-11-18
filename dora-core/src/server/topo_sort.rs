//! # Topological Sort
//!
//! Provides a tree structure for holding dependency graphs. Using this tree,
//! we can do a DFS and, assuming the graph is acyclic, generate a list of
//! nodes in the order of which they need to be evaluated.   Ex.
//!
//!   ```rust
//! use dora_core::server::topo_sort::DependencyTree;
//! use std::any::{Any, TypeId};
//!
//! struct A;
//! struct B;
//! struct C;
//! struct D;
//! struct E;
//! let mut tree = DependencyTree::new();
//! tree.add(TypeId::of::<A>(), Box::new(A) as Box<dyn Any>, &[]);
//! tree.add(
//!     TypeId::of::<B>(),
//!     Box::new(B) as Box<dyn Any>,
//!     &[TypeId::of::<A>()],
//! );
//! tree.add(
//!     TypeId::of::<C>(),
//!     Box::new(C) as Box<dyn Any>,
//!     &[TypeId::of::<A>()],
//! );
//! tree.add(
//!     TypeId::of::<D>(),
//!     Box::new(D) as Box<dyn Any>,
//!     &[TypeId::of::<B>(), TypeId::of::<C>(), TypeId::of::<E>()],
//! );
//! tree.add(
//!     TypeId::of::<E>(),
//!     Box::new(E) as Box<dyn Any>,
//!     &[TypeId::of::<A>(), TypeId::of::<C>()],
//! );
//! // returns the deps in a vec
//! let deps = tree.topological_sort().unwrap();
//! let type_ids = deps.iter().map(|x| (**x).type_id()).collect::<Vec<_>>();
//!
//! assert_eq!(
//!     &type_ids,
//!     &[
//!         TypeId::of::<A>(),
//!         TypeId::of::<C>(),
//!         TypeId::of::<E>(),
//!         TypeId::of::<B>(),
//!         TypeId::of::<D>(),
//!     ]
//! );
//! ```
//!
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    fmt::Debug,
    hash::{Hash, Hasher},
};
use thiserror::Error;

/// Used to keep track of # of parents and child nodes in `DependencyTree`
#[derive(Debug)]
struct Node<T> {
    num_parents: usize,
    children: Vec<T>,
    value: T,
}

impl<T> PartialEq for Node<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T> Eq for Node<T> where T: Eq {}

impl<T> Hash for Node<T>
where
    T: Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<T> Node<T> {
    fn new(value: T) -> Self {
        Self {
            num_parents: 0,
            children: Vec::new(),
            value,
        }
    }
}

/// Provides methods to create a list of `T` which satisfies it's dependencies
/// based on topological sort for graphs
#[derive(Debug)]
pub struct DependencyTree<K, T> {
    items: HashMap<K, T>,
    dep_tree: HashMap<K, Node<K>>,
}

impl<K, T> Default for DependencyTree<K, T> {
    fn default() -> Self {
        Self {
            items: HashMap::new(),
            dep_tree: HashMap::new(),
        }
    }
}

impl<K, T> DependencyTree<K, T>
where
    K: Eq + Hash + Clone,
{
    /// Create a new `DependencyTree`
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an item at some key index with a list of dependents
    pub fn add<U>(&mut self, key: K, item: T, input: U)
    where
        U: AsRef<[K]>,
    {
        let name = key;
        self.items.insert(name.clone(), item);

        let mut num_parents: usize = 0;
        for parent in input.as_ref() {
            if parent == &name {
                continue;
            }
            self.dep_tree
                .entry(parent.clone())
                .or_insert_with(|| Node::new(parent.clone()))
                .children
                .push(name.clone());
            num_parents += 1;
        }
        self.dep_tree
            .entry(name.clone())
            .or_insert_with(|| Node {
                value: name.clone(),
                children: Vec::new(),
                num_parents,
            })
            .num_parents = num_parents;
    }

    fn _topological_sort(mut dep_tree: HashMap<K, Node<K>>) -> Result<Vec<K>, TopoSortError> {
        // track visited nodes
        let mut visited = dep_tree.keys().cloned().collect::<HashSet<_>>();
        // our queue for DFS
        let mut queue = dep_tree
            .iter()
            .filter(|(_, v)| v.num_parents == 0)
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        // topologically sorted nodes
        let mut ret = Vec::new();

        while let Some(cur) = queue.pop() {
            for children in dep_tree
                .get_mut(&cur)
                .ok_or(TopoSortError::NoEntries)?
                .children
                .drain(0..)
                .collect::<Vec<_>>()
            {
                let child = dep_tree
                    .get_mut(&children)
                    .ok_or(TopoSortError::NoEntries)?;
                child.num_parents -= 1;
                if child.num_parents == 0 {
                    queue.push(child.value.clone());
                }
            }
            visited.remove(&cur);
            ret.push(cur);
        }

        if !visited.is_empty() {
            Err(TopoSortError::CycleDetected)
        } else {
            Ok(ret)
        }
    }

    /// Topologically sort the items in our adjecency map, producing a list of
    /// items in order they need be run according to their dependencies.
    /// Ex.
    /// A -> B -> D
    ///   -> C
    /// output: [A, C, B, D] or [A, B, C, D]
    /// Will return Err if there is a cycle or if the map is malformed
    pub fn topological_sort(self) -> Result<Vec<T>, TopoSortError> {
        let DependencyTree {
            mut items,
            dep_tree,
        } = self;

        Ok(DependencyTree::<K, T>::_topological_sort(dep_tree)?
            .into_iter()
            .flat_map(|id| items.remove(&id))
            .collect::<Vec<_>>())
    }
}

impl<K, T> Extend<(T, K, Vec<K>)> for DependencyTree<K, T>
where
    K: Eq + Hash + Clone,
    T: Any,
{
    fn extend<I: IntoIterator<Item = (T, K, Vec<K>)>>(&mut self, iter: I) {
        for (item, ty_id, deps) in iter.into_iter() {
            self.add(ty_id, item, deps);
        }
    }
}

/// Error type for topographical sort
#[derive(Error, Copy, Clone, Debug)]
pub enum TopoSortError {
    /// cycle found
    #[error("cycle detected in dependency map")]
    CycleDetected,
    /// no entries
    #[error("entry not found in dependency map")]
    NoEntries,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    struct A;
    struct B;
    struct C;
    struct D;
    struct E;

    #[test]
    fn simple_tree_stack() {
        let mut tree = DependencyTree::new();
        tree.add(TypeId::of::<A>(), Box::new(A) as Box<dyn Any>, []);
        tree.add(
            TypeId::of::<B>(),
            Box::new(B) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        tree.add(
            TypeId::of::<C>(),
            Box::new(C) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        tree.add(
            TypeId::of::<D>(),
            Box::new(D) as Box<dyn Any>,
            [TypeId::of::<B>(), TypeId::of::<C>()],
        );
        let DependencyTree { dep_tree, .. } = tree;
        let deps = DependencyTree::<_, Box<dyn Any>>::_topological_sort(dep_tree).unwrap();
        assert_eq!(
            &deps,
            &[
                TypeId::of::<A>(),
                TypeId::of::<C>(),
                TypeId::of::<B>(),
                TypeId::of::<D>()
            ]
        );
    }

    #[test]
    fn find_cycle() {
        let mut tree = DependencyTree::new();
        tree.add(TypeId::of::<A>(), Box::new(A) as Box<dyn Any>, []);
        tree.add(
            TypeId::of::<B>(),
            Box::new(B) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        // this node contains a cycle with D
        tree.add(
            TypeId::of::<C>(),
            Box::new(C) as Box<dyn Any>,
            [TypeId::of::<A>(), TypeId::of::<D>()],
        );
        tree.add(
            TypeId::of::<D>(),
            Box::new(D) as Box<dyn Any>,
            [TypeId::of::<B>(), TypeId::of::<C>()],
        );
        let DependencyTree { dep_tree, .. } = tree;
        let deps = DependencyTree::<_, Box<dyn Any>>::_topological_sort(dep_tree);
        assert!(deps.is_err());
    }

    #[test]
    fn simple_tree_two() {
        let mut tree = DependencyTree::new();
        tree.add(TypeId::of::<A>(), Box::new(A) as Box<dyn Any>, []);
        tree.add(
            TypeId::of::<B>(),
            Box::new(B) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        tree.add(
            TypeId::of::<C>(),
            Box::new(C) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        tree.add(
            TypeId::of::<D>(),
            Box::new(D) as Box<dyn Any>,
            [TypeId::of::<B>(), TypeId::of::<C>(), TypeId::of::<E>()],
        );
        tree.add(
            TypeId::of::<E>(),
            Box::new(E) as Box<dyn Any>,
            [TypeId::of::<A>(), TypeId::of::<C>()],
        );
        let DependencyTree { dep_tree, .. } = tree;
        let deps = DependencyTree::<_, Box<dyn Any>>::_topological_sort(dep_tree).unwrap();
        assert_eq!(
            &deps,
            &[
                TypeId::of::<A>(),
                TypeId::of::<C>(),
                TypeId::of::<E>(),
                TypeId::of::<B>(),
                TypeId::of::<D>(),
            ]
        );
    }

    #[test]
    fn simple_tree_two_insertion_order() {
        let mut tree = DependencyTree::new();
        tree.add(
            TypeId::of::<D>(),
            Box::new(D) as Box<dyn Any>,
            [TypeId::of::<B>(), TypeId::of::<C>(), TypeId::of::<E>()],
        );
        tree.add(TypeId::of::<A>(), Box::new(A) as Box<dyn Any>, []);
        tree.add(
            TypeId::of::<C>(),
            Box::new(C) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        tree.add(
            TypeId::of::<E>(),
            Box::new(E) as Box<dyn Any>,
            [TypeId::of::<A>(), TypeId::of::<C>()],
        );
        tree.add(
            TypeId::of::<B>(),
            Box::new(B) as Box<dyn Any>,
            [TypeId::of::<A>()],
        );
        let DependencyTree { dep_tree, .. } = tree;
        let deps = DependencyTree::<_, Box<dyn Any>>::_topological_sort(dep_tree).unwrap();

        assert_eq!(
            &deps,
            &[
                TypeId::of::<A>(),
                TypeId::of::<B>(),
                TypeId::of::<C>(),
                TypeId::of::<E>(),
                TypeId::of::<D>(),
            ]
        );
    }

    #[test]
    fn runtest() {}
}
