use crate::queries_repository::QueryType;
use mongodb::bson::Document;
use serde::{Deserialize, Serialize};

/// A prepared MongoDB operation. Unlike `SqlQuery` (which carries positional placeholders
/// resolved at execution time), operations here are fully resolved at generation time: any
/// random ids/values are baked into the `Document`/pipeline, mirroring how `generate-queries`
/// already persists one concrete, ready-to-run query per line for every vendor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MongoOperation {
    /// `db.<collection>.findOne(filter)`
    Find { collection: String, filter: Document },
    /// `db.<collection>.aggregate(pipeline)`
    Aggregate {
        collection: String,
        pipeline: Vec<Document>,
    },
    /// `db.<collection>.insertOne(document)`
    InsertOne { collection: String, document: Document },
    /// `db.<collection>.updateOne(filter, update, {upsert})`
    UpdateOne {
        collection: String,
        filter: Document,
        update: Document,
        upsert: bool,
    },
    /// `db.<collection>.deleteOne(filter)`
    DeleteOne { collection: String, filter: Document },
}

/// A prepared MongoDB query with a stable catalog id, mirroring `sql_query::PreparedSqlQuery`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedMongoQuery {
    #[serde(default)]
    pub q_id: u16,
    pub q_name: String,
    pub q_type: QueryType,
    pub operation: MongoOperation,
}

impl PreparedMongoQuery {
    pub fn new(
        q_id: u16,
        q_name: String,
        q_type: QueryType,
        operation: MongoOperation,
    ) -> Self {
        Self {
            q_id,
            q_name,
            q_type,
            operation,
        }
    }
}
