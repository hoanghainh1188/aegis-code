/// One page of a list, by keyset: no list query is unbounded (ADR 0003 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    after: Option<i64>,
    limit: u32,
}

impl Page {
    pub const MAX_LIMIT: u32 = 200;

    /// The first `limit` items, within `1..=MAX_LIMIT`.
    pub fn first(limit: u32) -> Self {
        Self {
            after: None,
            limit: limit.clamp(1, Self::MAX_LIMIT),
        }
    }

    /// The items after the one with id `cursor`, in the list's order.
    pub fn after(cursor: i64, limit: u32) -> Self {
        Self {
            after: Some(cursor),
            ..Self::first(limit)
        }
    }

    pub fn limit(self) -> u32 {
        self.limit
    }

    pub fn cursor(self) -> Option<i64> {
        self.after
    }

    /// One more row than the page holds, to tell whether another follows.
    pub(crate) fn fetch(self) -> i64 {
        i64::from(self.limit) + 1
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageOf<T> {
    pub items: Vec<T>,
    next: Option<Page>,
}

impl<T> PageOf<T> {
    /// `rows` as fetched with [`Page::fetch`]; `id` gives the cursor.
    pub(crate) fn new(mut rows: Vec<T>, page: Page, id: impl Fn(&T) -> i64) -> Self {
        let more = rows.len() > page.limit as usize;
        rows.truncate(page.limit as usize);
        let next = rows
            .last()
            .filter(|_| more)
            .map(|last| Page::after(id(last), page.limit));
        Self { items: rows, next }
    }

    pub fn next_page(&self) -> Option<Page> {
        self.next
    }
}
