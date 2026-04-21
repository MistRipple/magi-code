use std::cmp::Ordering;

pub struct MinHeap<T> {
    heap: Vec<T>,
    compare: fn(&T, &T) -> Ordering,
    capacity: usize,
}

impl<T> MinHeap<T> {
    pub fn new(capacity: usize, compare: fn(&T, &T) -> Ordering) -> Self {
        Self {
            heap: Vec::with_capacity(capacity),
            compare,
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn peek(&self) -> Option<&T> {
        self.heap.first()
    }

    pub fn push(&mut self, item: T) {
        if self.heap.len() < self.capacity {
            self.heap.push(item);
            self.sift_up(self.heap.len() - 1);
        } else if !self.heap.is_empty()
            && (self.compare)(&item, &self.heap[0]) == Ordering::Greater
        {
            self.heap[0] = item;
            self.sift_down(0);
        }
    }

    pub fn into_sorted_desc(mut self) -> Vec<T> {
        self.heap.sort_by(|a, b| (self.compare)(b, a));
        self.heap
    }

    fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) >> 1;
            if (self.compare)(&self.heap[index], &self.heap[parent]) == Ordering::Less {
                self.heap.swap(index, parent);
                index = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        let n = self.heap.len();
        loop {
            let mut smallest = index;
            let left = 2 * index + 1;
            let right = 2 * index + 2;

            if left < n
                && (self.compare)(&self.heap[left], &self.heap[smallest]) == Ordering::Less
            {
                smallest = left;
            }
            if right < n
                && (self.compare)(&self.heap[right], &self.heap[smallest]) == Ordering::Less
            {
                smallest = right;
            }

            if smallest != index {
                self.heap.swap(index, smallest);
                index = smallest;
            } else {
                break;
            }
        }
    }
}
