use crate::mongo_query::{MongoOperation, PreparedMongoQuery};
use crate::queries_repository::{QueryCatalogEntry, QueryCoverageProfile, QueryType};
use mongodb::bson::{doc, Bson, Document};
use rand::prelude::IndexedRandom;
use std::collections::HashMap;

type MongoOperationFn = Box<dyn Fn() -> MongoOperation + Send + Sync>;
type MongoOperationEntry = (String, QueryType, MongoOperationFn);

struct RandomUtil {
    vertices: i32,
}

impl RandomUtil {
    fn random_vertex(&self) -> i32 {
        rand::random_range(1..=self.vertices)
    }

    fn random_path(&self) -> (i32, i32) {
        let start = self.random_vertex();
        let mut end = self.random_vertex();
        while end == start {
            end = self.random_vertex();
        }
        (start, end)
    }
}

struct MongoOperationGenerator {
    query_type: QueryType,
    generator: MongoOperationFn,
}

impl MongoOperationGenerator {
    fn generate(&self) -> MongoOperation {
        (self.generator)()
    }
}

/// Mongo analogue of `postgres_queries_repository::PostgresQueriesRepository`. There is no
/// `Flavour` concept (single aggregation-pipeline dialect) and no algorithm-query sub-pool
/// (Mongo has no equivalent procedures).
pub struct MongoQueriesRepository {
    read_queries: HashMap<String, MongoOperationGenerator>,
    write_queries: HashMap<String, MongoOperationGenerator>,
    read_query_names: Vec<String>,
    write_query_names: Vec<String>,
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl MongoQueriesRepository {
    fn new() -> Self {
        MongoQueriesRepository {
            read_queries: HashMap::new(),
            write_queries: HashMap::new(),
            read_query_names: Vec::new(),
            write_query_names: Vec::new(),
            name_to_id: HashMap::new(),
            catalog: Vec::new(),
        }
    }

    fn add_with_id(
        &mut self,
        id: u16,
        name: String,
        query_type: QueryType,
        generator: MongoOperationFn,
    ) {
        self.name_to_id.insert(name.clone(), id);
        self.catalog.push(QueryCatalogEntry {
            id,
            name: name.clone(),
            q_type: query_type,
        });

        match query_type {
            QueryType::Read => {
                self.read_query_names.push(name.clone());
                self.read_queries
                    .insert(name, MongoOperationGenerator { query_type, generator });
            }
            QueryType::Write => {
                self.write_query_names.push(name.clone());
                self.write_queries
                    .insert(name, MongoOperationGenerator { query_type, generator });
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    fn random_query_from_pool(
        &self,
        queries: &HashMap<String, MongoOperationGenerator>,
        query_names: &[String],
    ) -> Option<PreparedMongoQuery> {
        let mut rng = rand::rng();
        let key = query_names.choose(&mut rng)?;
        let generator = queries.get(key)?;
        let q_id = *self.name_to_id.get(key).unwrap_or(&0);
        Some(PreparedMongoQuery::new(
            q_id,
            key.clone(),
            generator.query_type,
            generator.generate(),
        ))
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedMongoQuery> {
        let (queries, query_names) = match query_type {
            QueryType::Read => (&self.read_queries, &self.read_query_names),
            QueryType::Write => (&self.write_queries, &self.write_query_names),
        };
        self.random_query_from_pool(queries, query_names)
    }
}

struct MongoQueriesRepositoryBuilder {
    vertices: i32,
    queries: Vec<MongoOperationEntry>,
}

impl MongoQueriesRepositoryBuilder {
    fn new(vertices: i32) -> Self {
        MongoQueriesRepositoryBuilder {
            vertices,
            queries: Vec::new(),
        }
    }

    fn add_query<F>(
        mut self,
        name: impl Into<String>,
        query_type: QueryType,
        generator: F,
    ) -> Self
    where
        F: Fn(&RandomUtil) -> MongoOperation + Send + Sync + 'static,
    {
        let vertices = self.vertices;
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move || {
                let random = RandomUtil { vertices };
                generator(&random)
            }),
        ));
        self
    }

    fn build(self) -> MongoQueriesRepository {
        let mut repo = MongoQueriesRepository::new();
        for (idx, (name, query_type, generator)) in self.queries.into_iter().enumerate() {
            repo.add_with_id(idx as u16, name, query_type, generator);
        }
        repo
    }
}

/// Traverse `friend_edges` starting from `seed`, following `src -> dst` edges up to `max_depth`
/// recursive hops (depth 0 = direct edges out of `seed`). Mirrors the shape of a Cypher
/// `(seed)-->()...-->(n)` chain, using `$graphLookup`'s BFS-like traversal.
fn graph_lookup_stage(
    max_depth: i32,
    restrict_with_match: Option<Document>,
) -> Document {
    let mut stage = doc! {
        "from": "friend_edges",
        "startWith": "$_id",
        "connectFromField": "dst",
        "connectToField": "src",
        "as": "reachable",
        "maxDepth": max_depth,
        "depthField": "depth",
    };
    if let Some(restrict) = restrict_with_match {
        stage.insert("restrictSearchWithMatch", restrict);
    }
    doc! { "$graphLookup": stage }
}

/// Expansion pipeline: from `seed`, follow exactly `hops` edges (`hops - 1` recursive
/// `$graphLookup` steps), optionally joining back to `users` to apply an `age >= 18` filter.
/// Mirrors `aggregate_expansion_N[_with_filter]` / `neighbours_2[_with_filter]`.
fn expansion_pipeline(
    seed: i32,
    hops: i32,
    with_filter: bool,
    distinct: bool,
) -> Vec<Document> {
    let mut pipeline = vec![
        doc! { "$match": { "_id": seed } },
        graph_lookup_stage(hops - 1, None),
        doc! { "$unwind": "$reachable" },
        doc! { "$match": { "reachable.depth": hops - 1 } },
    ];

    if with_filter {
        pipeline.push(doc! {
            "$lookup": {
                "from": "users",
                "localField": "reachable.dst",
                "foreignField": "_id",
                "as": "u",
            }
        });
        pipeline.push(doc! { "$unwind": "$u" });
        pipeline.push(doc! { "$match": { "u.age": { "$gte": 18 } } });
        pipeline.push(doc! { "$project": { "_id": "$u._id" } });
    } else {
        pipeline.push(doc! { "$project": { "_id": "$reachable.dst" } });
    }

    if distinct {
        pipeline.push(doc! { "$group": { "_id": "$_id" } });
    }

    pipeline
}

fn expansion_with_data_pipeline(
    seed: i32,
    hops: i32,
    with_filter: bool,
) -> Vec<Document> {
    let mut pipeline = vec![
        doc! { "$match": { "_id": seed } },
        graph_lookup_stage(hops - 1, None),
        doc! { "$unwind": "$reachable" },
        doc! { "$match": { "reachable.depth": hops - 1 } },
        doc! {
            "$lookup": {
                "from": "users",
                "localField": "reachable.dst",
                "foreignField": "_id",
                "as": "u",
            }
        },
        doc! { "$unwind": "$u" },
    ];
    if with_filter {
        pipeline.push(doc! { "$match": { "u.age": { "$gte": 18 } } });
    }
    pipeline.push(doc! { "$replaceRoot": { "newRoot": "$u" } });
    pipeline
}

/// Mongo analogue of `postgres_queries_repository::PostgresUsersQueriesRepository`.
pub struct MongoUsersQueriesRepository {
    repo: MongoQueriesRepository,
}

impl MongoUsersQueriesRepository {
    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.repo.catalog()
    }

    pub fn random_queries(
        self,
        count: usize,
        write_ratio: f32,
    ) -> Box<dyn Iterator<Item = PreparedMongoQuery> + Send + Sync> {
        Box::new((0..count).filter_map(move |_| self.random_query(write_ratio)))
    }

    pub fn random_query(
        &self,
        write_ratio: f32,
    ) -> Option<PreparedMongoQuery> {
        let write_ratio = write_ratio.clamp(0.0, 1.0);
        if rand::random::<f32>() < write_ratio {
            self.repo
                .random_query(QueryType::Write)
                .or_else(|| self.repo.random_query(QueryType::Read))
        } else {
            self.repo
                .random_query(QueryType::Read)
                .or_else(|| self.repo.random_query(QueryType::Write))
        }
    }

    /// Build the Mongo query catalog. Algorithm queries (pagerank/max-flow/MSF/harmonic), vector
    /// /fulltext smoke queries, and `entity_path_introspection` have no aggregation-pipeline
    /// equivalent and are always omitted, same as Postgres. Additionally, `shortest_path`,
    /// `shortest_path_with_filter`, `all_shortest_paths_len`, and `pattern_cycle` are omitted:
    /// `$graphLookup` returns an unordered, deduplicated reachable set (first-seen depth per
    /// node), which can answer reachability/hop-count questions but cannot enumerate distinct
    /// paths (needed for true shortest-path) or verify a full cyclic pattern's intermediate
    /// nodes -- these remain Postgres-only per the capability matrix in
    /// `QUERY_EXPLANATIONS_AND_SAMPLES.md`. Callers are expected to reject the
    /// `fixture-dependent` profile for Mongo before reaching this constructor.
    pub fn new(
        vertices: i32,
        _edges: i32,
        query_coverage_profile: QueryCoverageProfile,
    ) -> MongoUsersQueriesRepository {
        let mut builder = MongoQueriesRepositoryBuilder::new(vertices)
            .add_query("single_vertex_read", QueryType::Read, |random| {
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("single_vertex_write", QueryType::Write, |random| {
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                    update: doc! { "$setOnInsert": { "created_at": Bson::from(chrono_now()) } },
                    upsert: true,
                }
            })
            .add_query("single_vertex_update", QueryType::Write, |random| {
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                    update: doc! { "$set": { "rpc_social_credit": random.random_vertex() } },
                    upsert: false,
                }
            })
            .add_query("single_edge_update", QueryType::Write, |random| {
                // No SQL-style "ORDER BY random() LIMIT 1" equivalent for a single update
                // command; `$sample` + `$merge` is the standard Mongo idiom for "update a
                // randomly chosen document" via an aggregation pipeline.
                MongoOperation::Aggregate {
                    collection: "friend_edges".to_string(),
                    pipeline: vec![
                        doc! { "$sample": { "size": 1 } },
                        doc! { "$set": { "color": random.random_vertex() } },
                        doc! { "$merge": { "into": "friend_edges", "whenMatched": "merge", "whenNotMatched": "discard" } },
                    ],
                }
            })
            .add_query("single_edge_write", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                MongoOperation::UpdateOne {
                    collection: "friend_edges".to_string(),
                    filter: doc! { "src": from, "dst": to },
                    update: doc! {
                        "$setOnInsert": { "bench_capacity": bench_capacity_placeholder(from, to) },
                        "$set": { "touch": Bson::from(chrono_now()) },
                    },
                    upsert: true,
                }
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 1, false, false),
                }
            })
            .add_query("aggregate_expansion_1_with_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 1, true, false),
                }
            })
            .add_query("aggregate_expansion_2", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 2, false, true),
                }
            })
            .add_query("aggregate_expansion_2_with_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 2, true, true),
                }
            })
            .add_query("aggregate_expansion_3", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 3, false, true),
                }
            })
            .add_query("aggregate_expansion_3_with_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 3, true, true),
                }
            })
            .add_query("aggregate_expansion_4", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 4, false, true),
                }
            })
            .add_query("aggregate_expansion_4_with_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 4, true, true),
                }
            })
            .add_query("aggregate_age", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![doc! { "$group": { "_id": Bson::Null, "avg_age": { "$avg": "$age" } } }],
                }
            })
            .add_query("aggregate_age_distinct", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$group": { "_id": "$age" } },
                        doc! { "$count": "distinct_ages" },
                    ],
                }
            })
            .add_query("aggregate_age_filtered", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "age": { "$gte": 18 } } },
                        doc! { "$group": { "_id": Bson::Null, "avg_age": { "$avg": "$age" } } },
                    ],
                }
            })
            .add_query("aggregate_count_users", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![doc! { "$count": "cnt" }],
                }
            })
            .add_query("aggregate_age_min_max_avg", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![doc! { "$group": {
                        "_id": Bson::Null,
                        "min_age": { "$min": "$age" },
                        "max_age": { "$max": "$age" },
                        "avg_age": { "$avg": "$age" },
                    } }],
                }
            })
            .add_query("neighbours_2", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 2, false, false),
                }
            })
            .add_query("neighbours_2_with_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_pipeline(random.random_vertex(), 2, true, false),
                }
            })
            .add_query("neighbours_2_with_data", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_with_data_pipeline(random.random_vertex(), 2, false),
                }
            })
            .add_query("neighbours_2_with_data_and_filter", QueryType::Read, |random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: expansion_with_data_pipeline(random.random_vertex(), 2, true),
                }
            })
            .add_query("pattern_long", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        graph_lookup_stage(3, None),
                        doc! { "$unwind": "$reachable" },
                        doc! { "$match": { "reachable.depth": 3 } },
                        doc! { "$project": { "a_id": seed, "b_id": "$reachable.dst" } },
                    ],
                }
            })
            .add_query("pattern_short", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        graph_lookup_stage(1, None),
                        doc! { "$unwind": "$reachable" },
                        doc! { "$match": { "reachable.depth": 1 } },
                        doc! { "$project": { "a_id": seed, "b_id": "$reachable.dst" } },
                    ],
                }
            })
            .add_query("vertex_on_label_property", QueryType::Read, |random| {
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("vertex_on_label_property_index", QueryType::Read, |random| {
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("vertex_on_property", QueryType::Read, |random| {
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("value_join", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$lookup": {
                            "from": "users",
                            "let": { "age": "$age" },
                            "pipeline": [ doc! { "$match": { "$expr": { "$eq": ["$age", "$$age"] } } } ],
                            "as": "matches",
                        } },
                        doc! { "$unwind": "$matches" },
                        doc! { "$project": { "_id": "$matches._id" } },
                    ],
                }
            })
            .add_query("value_join_cnt", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$lookup": {
                            "from": "users",
                            "let": { "age": "$age" },
                            "pipeline": [ doc! { "$match": { "$expr": { "$eq": ["$age", "$$age"] } } } ],
                            "as": "matches",
                        } },
                        doc! { "$project": { "cnt": { "$size": "$matches" } } },
                    ],
                }
            })
            .add_query("order_by_age", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$sort": { "age": 1, "_id": 1 } },
                        doc! { "$project": { "_id": 1, "age": 1 } },
                    ],
                }
            })
            .add_query("unwind_rows", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$project": { "vals": [seed, seed + 1, seed + 2] } },
                        doc! { "$unwind": "$vals" },
                    ],
                }
            })
            .add_query("var_len_friends", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        graph_lookup_stage(1, None),
                        doc! { "$unwind": "$reachable" },
                        doc! { "$group": { "_id": "$reachable.dst" } },
                    ],
                }
            })
            .add_query("optional_friend", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$lookup": { "from": "friend_edges", "localField": "_id", "foreignField": "src", "as": "edges" } },
                        doc! { "$unwind": { "path": "$edges", "preserveNullAndEmptyArrays": true } },
                        doc! { "$project": { "a_id": "$_id", "b_id": "$edges.dst" } },
                    ],
                }
            })
            .add_query("call_subquery", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$lookup": { "from": "friend_edges", "localField": "_id", "foreignField": "src", "as": "sub" } },
                        doc! { "$unwind": "$sub" },
                        doc! { "$project": { "bid": "$sub.dst" } },
                    ],
                }
            })
            .add_query("id_seek", QueryType::Read, |random| {
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("id_range_scan", QueryType::Read, |random| {
                let start = random.random_vertex();
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": { "$gte": start, "$lt": start + 100 } },
                }
            })
            .add_query("merge_user_insert_path", QueryType::Write, |random| {
                let insert_id = random.vertices.saturating_add(random.random_vertex());
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": insert_id },
                    update: doc! { "$setOnInsert": {
                        "created_at": Bson::from(chrono_now()),
                        "age": random.random_vertex(),
                    } },
                    upsert: true,
                }
            })
            .add_query("merge_user_upsert_existing", QueryType::Write, |random| {
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                    update: doc! {
                        "$set": { "age": random.random_vertex(), "last_seen": Bson::from(chrono_now()) },
                        "$setOnInsert": { "created_at": Bson::from(chrono_now()) },
                    },
                    upsert: true,
                }
            })
            .add_query("merge_friend_edge_upsert", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                MongoOperation::UpdateOne {
                    collection: "friend_edges".to_string(),
                    filter: doc! { "src": from, "dst": to },
                    update: doc! {
                        "$setOnInsert": { "since": Bson::from(chrono_now()), "bench_capacity": bench_capacity_placeholder(from, to) },
                        "$set": { "touch": Bson::from(chrono_now()) },
                    },
                    upsert: true,
                }
            })
            .add_query("detach_delete_user", QueryType::Write, |random| {
                // Mongo has no FK/cascade concept: unlike Postgres's `ON DELETE CASCADE`, this
                // only removes the user document; any `friend_edges` referencing it are left in
                // place (documented limitation, see QUERY_EXPLANATIONS_AND_SAMPLES.md).
                MongoOperation::DeleteOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                }
            })
            .add_query("remove_user_property_and_label", QueryType::Write, |random| {
                // Mongo has no label concept; this drops only the property-removal semantics.
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                    update: doc! { "$unset": { "rpc_social_credit": "" } },
                    upsert: false,
                }
            })
            .add_query("foreach_loop_mutation", QueryType::Write, |random| {
                // Approximated as a single terminal assignment (equivalent end state to the
                // Cypher `FOREACH (x IN [1,2,3] | SET u.loop_counter = x)`).
                MongoOperation::UpdateOne {
                    collection: "users".to_string(),
                    filter: doc! { "_id": random.random_vertex() },
                    update: doc! { "$set": { "loop_counter": 3 } },
                    upsert: false,
                }
            })
            .add_query("union_all_ids", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$project": { "uid": "$_id" } },
                        doc! { "$unionWith": { "coll": "users", "pipeline": [
                            doc! { "$match": { "_id": { "$lt": 10 } } },
                            doc! { "$project": { "uid": "$_id" } },
                        ] } },
                    ],
                }
            })
            .add_query("union_distinct_ids", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        doc! { "$project": { "uid": "$_id" } },
                        doc! { "$unionWith": { "coll": "users", "pipeline": [
                            doc! { "$match": { "_id": seed } },
                            doc! { "$project": { "uid": "$_id" } },
                        ] } },
                        doc! { "$group": { "_id": "$uid" } },
                    ],
                }
            })
            .add_query("var_len_with_edge_where_filter", QueryType::Read, |random| {
                let seed = random.random_vertex();
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![
                        doc! { "$match": { "_id": seed } },
                        graph_lookup_stage(2, Some(doc! { "bench_capacity": { "$gte": 1 } })),
                        doc! { "$unwind": "$reachable" },
                        doc! { "$group": { "_id": "$reachable.dst" } },
                        doc! { "$count": "cnt" },
                    ],
                }
            })
            .add_query("count_users_plain", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "users".to_string(),
                    pipeline: vec![doc! { "$count": "cnt" }],
                }
            })
            .add_query("count_friend_edges_plain", QueryType::Read, |_random| {
                MongoOperation::Aggregate {
                    collection: "friend_edges".to_string(),
                    pipeline: vec![doc! { "$count": "cnt" }],
                }
            })
            .add_query("indexed_or_predicate", QueryType::Read, |random| {
                let (id1, id2) = random.random_path();
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "$or": [ { "_id": id1 }, { "_id": id2 } ] },
                }
            })
            .add_query("indexed_in_list_predicate", QueryType::Read, |random| {
                let id1 = random.random_vertex();
                let id2 = random.random_vertex();
                let id3 = random.random_vertex();
                let id4 = random.random_vertex();
                MongoOperation::Find {
                    collection: "users".to_string(),
                    filter: doc! { "_id": { "$in": [id1, id2, id3, id4] } },
                }
            });

        if query_coverage_profile.includes_extended_core() {
            builder = builder
                .add_query("exact_5_hop_traverse_count", QueryType::Read, |random| {
                    let seed = random.random_vertex();
                    MongoOperation::Aggregate {
                        collection: "users".to_string(),
                        pipeline: vec![
                            doc! { "$match": { "_id": seed } },
                            graph_lookup_stage(4, None),
                            doc! { "$unwind": "$reachable" },
                            doc! { "$match": { "reachable.depth": 4 } },
                            doc! { "$count": "cnt" },
                        ],
                    }
                })
                .add_query("exact_6_hop_traverse_count", QueryType::Read, |random| {
                    let seed = random.random_vertex();
                    MongoOperation::Aggregate {
                        collection: "users".to_string(),
                        pipeline: vec![
                            doc! { "$match": { "_id": seed } },
                            graph_lookup_stage(5, None),
                            doc! { "$unwind": "$reachable" },
                            doc! { "$match": { "reachable.depth": 5 } },
                            doc! { "$count": "cnt" },
                        ],
                    }
                })
                .add_query("temporal_spatial_roundtrip", QueryType::Read, |_random| {
                    // No `$geoNear`/PostGIS-style dependency: distance is computed manually via
                    // the spherical law of cosines using Mongo's trigonometry aggregation
                    // operators ($sin/$cos/$acos/$degreesToRadians, added in MongoDB 4.2), and
                    // `$documents` (MongoDB 6.0+) seeds a single input row without touching a
                    // real collection.
                    MongoOperation::Aggregate {
                        collection: "users".to_string(),
                        pipeline: vec![
                            doc! { "$documents": [ {} ] },
                            doc! { "$project": {
                                "d": { "$dateFromString": { "dateString": "2024-01-01" } },
                                "dur_hours": 51,
                                "dist": {
                                    "$multiply": [
                                        6371000,
                                        { "$acos": {
                                            "$add": [
                                                { "$multiply": [
                                                    { "$cos": { "$degreesToRadians": 32.1 } },
                                                    { "$cos": { "$degreesToRadians": 32.2 } },
                                                    { "$cos": { "$subtract": [
                                                        { "$degreesToRadians": 34.9 },
                                                        { "$degreesToRadians": 34.8 },
                                                    ] } },
                                                ] },
                                                { "$multiply": [
                                                    { "$sin": { "$degreesToRadians": 32.1 } },
                                                    { "$sin": { "$degreesToRadians": 32.2 } },
                                                ] },
                                            ],
                                        } },
                                    ],
                                },
                            } },
                        ],
                    }
                });
        }

        let repo = builder.build();
        MongoUsersQueriesRepository { repo }
    }
}

/// Deterministic placeholder mirroring `data_prep::bench_capacity`, kept local to avoid a
/// dependency cycle; values only need to be a stable function of (src, dst) for benchmark
/// purposes.
fn bench_capacity_placeholder(
    src: i32,
    dst: i32,
) -> i32 {
    crate::data_prep::bench_capacity(src as u64, dst as u64) as i32
}

/// Best-effort "now" as an RFC3339 string wrapped in a BSON `DateTime`-compatible value. Uses
/// `mongodb::bson::DateTime::now()` to avoid pulling in a separate `chrono` dependency.
fn chrono_now() -> mongodb::bson::DateTime {
    mongodb::bson::DateTime::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_catalog_excludes_unsupported_families() {
        let repo = MongoUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::Baseline);
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(!names.contains(&"algo_pagerank_summary".to_string()));
        assert!(!names.contains(&"vector_query_nodes_smoke".to_string()));
        assert!(!names.contains(&"entity_path_introspection".to_string()));
        assert!(!names.contains(&"shortest_path".to_string()));
        assert!(!names.contains(&"shortest_path_with_filter".to_string()));
        assert!(!names.contains(&"all_shortest_paths_len".to_string()));
        assert!(!names.contains(&"pattern_cycle".to_string()));
        assert!(names.contains(&"aggregate_expansion_2".to_string()));
        assert!(!names.contains(&"exact_5_hop_traverse_count".to_string()));
    }

    #[test]
    fn extended_core_adds_extra_queries() {
        let repo = MongoUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::ExtendedCore);
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"exact_5_hop_traverse_count".to_string()));
        assert!(names.contains(&"exact_6_hop_traverse_count".to_string()));
        assert!(names.contains(&"temporal_spatial_roundtrip".to_string()));
    }

    #[test]
    fn random_query_generation_produces_operations() {
        let repo = MongoUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::Baseline);
        for _ in 0..50 {
            let q = repo.random_query(0.5).expect("expected a query");
            match q.operation {
                MongoOperation::Find { collection, .. }
                | MongoOperation::Aggregate { collection, .. }
                | MongoOperation::InsertOne { collection, .. }
                | MongoOperation::UpdateOne { collection, .. }
                | MongoOperation::DeleteOne { collection, .. } => {
                    assert!(!collection.is_empty());
                }
            }
        }
    }
}
