// Sample: derive macro patterns
// Expected detections: Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default

use serde::{Deserialize, Serialize};

/// Newtype ID with common derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemId(pub u64);

/// Data transfer object with serde derives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: ItemId,
    pub name: String,
    pub category: Category,
    pub tags: Vec<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Category enum with multiple derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Book,
    Article,
    Video,
    Podcast,
}

/// Configuration with Default derive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterOptions {
    pub query: Option<String>,
    pub category: Option<Category>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "item-{}", self.0)
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Book => write!(f, "book"),
            Self::Article => write!(f, "article"),
            Self::Video => write!(f, "video"),
            Self::Podcast => write!(f, "podcast"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_id_display() {
        assert_eq!(ItemId(42).to_string(), "item-42");
    }

    #[test]
    fn test_category_serde_roundtrip() {
        let json = serde_json::to_string(&Category::Book).unwrap();
        assert_eq!(json, "\"book\"");
        let deserialized: Category = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, Category::Book);
    }

    #[test]
    fn test_filter_options_default() {
        let opts = FilterOptions::default();
        assert!(opts.query.is_none());
        assert!(opts.category.is_none());
        assert!(opts.limit.is_none());
    }

    #[test]
    fn test_item_dto_serialization() {
        let item = ItemDto {
            id: ItemId(1),
            name: "Test Item".into(),
            category: Category::Article,
            tags: vec!["rust".into(), "testing".into()],
            metadata: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("camelCase") || json.contains("name"));
        let deserialized: ItemDto = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, ItemId(1));
    }
}
