use crate::queries_repository::{QueryCatalogEntry, QueryCoverageProfile, QueryType};
use crate::sql_query::{PreparedSqlQuery, SqlQuery, SqlQueryBuilder};
use rand::prelude::IndexedRandom;
use std::collections::HashMap;

type SqlQueryFn = Box<dyn Fn() -> SqlQuery + Send + Sync>;
type SqlQueryEntry = (String, QueryType, SqlQueryFn);

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

struct SqlQueryGenerator {
    query_type: QueryType,
    generator: SqlQueryFn,
}

impl SqlQueryGenerator {
    fn generate(&self) -> SqlQuery {
        (self.generator)()
    }
}

/// Postgres analogue of `queries_repository::QueriesRepository`. There is no `Flavour` concept
/// (single SQL dialect) and no algorithm-query sub-pool (Postgres has no equivalent procedures).
pub struct PostgresQueriesRepository {
    read_queries: HashMap<String, SqlQueryGenerator>,
    write_queries: HashMap<String, SqlQueryGenerator>,
    read_query_names: Vec<String>,
    write_query_names: Vec<String>,
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl PostgresQueriesRepository {
    fn new() -> Self {
        PostgresQueriesRepository {
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
        generator: SqlQueryFn,
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
                    .insert(name, SqlQueryGenerator { query_type, generator });
            }
            QueryType::Write => {
                self.write_query_names.push(name.clone());
                self.write_queries
                    .insert(name, SqlQueryGenerator { query_type, generator });
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    fn random_query_from_pool(
        &self,
        queries: &HashMap<String, SqlQueryGenerator>,
        query_names: &[String],
    ) -> Option<PreparedSqlQuery> {
        let mut rng = rand::rng();
        let key = query_names.choose(&mut rng)?;
        let generator = queries.get(key)?;
        let q_id = *self.name_to_id.get(key).unwrap_or(&0);
        Some(PreparedSqlQuery::new(
            q_id,
            key.clone(),
            generator.query_type,
            generator.generate(),
        ))
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedSqlQuery> {
        let (queries, query_names) = match query_type {
            QueryType::Read => (&self.read_queries, &self.read_query_names),
            QueryType::Write => (&self.write_queries, &self.write_query_names),
        };
        self.random_query_from_pool(queries, query_names)
    }
}

struct PostgresQueriesRepositoryBuilder {
    vertices: i32,
    queries: Vec<SqlQueryEntry>,
}

impl PostgresQueriesRepositoryBuilder {
    fn new(vertices: i32) -> Self {
        PostgresQueriesRepositoryBuilder {
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
        F: Fn(&RandomUtil) -> SqlQuery + Send + Sync + 'static,
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

    fn build(self) -> PostgresQueriesRepository {
        let mut repo = PostgresQueriesRepository::new();
        for (idx, (name, query_type, generator)) in self.queries.into_iter().enumerate() {
            repo.add_with_id(idx as u16, name, query_type, generator);
        }
        repo
    }
}

pub struct PostgresUsersQueriesRepository {
    repo: PostgresQueriesRepository,
}

impl PostgresUsersQueriesRepository {
    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.repo.catalog()
    }

    pub fn random_queries(
        self,
        count: usize,
        write_ratio: f32,
    ) -> Box<dyn Iterator<Item = PreparedSqlQuery> + Send + Sync> {
        Box::new((0..count).filter_map(move |_| self.random_query(write_ratio)))
    }

    pub fn random_query(
        &self,
        write_ratio: f32,
    ) -> Option<PreparedSqlQuery> {
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

    /// Build the Postgres query catalog. Algorithm queries (pagerank/max-flow/MSF/harmonic),
    /// vector/fulltext smoke queries, and `entity_path_introspection` have no SQL equivalent and
    /// are intentionally omitted regardless of `query_coverage_profile`; callers are expected to
    /// reject the `fixture-dependent` profile for Postgres before reaching this constructor.
    pub fn new(
        vertices: i32,
        _edges: i32,
        query_coverage_profile: QueryCoverageProfile,
    ) -> PostgresUsersQueriesRepository {
        let mut builder = PostgresQueriesRepositoryBuilder::new(vertices)
            .add_query("single_vertex_read", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT * FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random| {
                SqlQueryBuilder::new()
                    .text("INSERT INTO users (id) VALUES ($1) ON CONFLICT (id) DO NOTHING RETURNING id")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_update", QueryType::Write, |random| {
                SqlQueryBuilder::new()
                    .text("UPDATE users SET rpc_social_credit = $2 WHERE id = $1 RETURNING id")
                    .param(random.random_vertex())
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("single_edge_update", QueryType::Write, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "UPDATE friend_edges SET color = $1, \
                         bench_capacity = COALESCE(bench_capacity, 1 + ((src_id * 31 + dst_id * 17) % 20)) \
                         WHERE (src_id, dst_id) = ( \
                            SELECT src_id, dst_id FROM friend_edges ORDER BY random() LIMIT 1 \
                         ) RETURNING src_id, dst_id",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("single_edge_write", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                SqlQueryBuilder::new()
                    .text(
                        "INSERT INTO friend_edges (src_id, dst_id, bench_capacity) \
                         VALUES ($1, $2, 1 + (($1 * 31 + $2 * 17) % 20)) \
                         ON CONFLICT (src_id, dst_id) DO UPDATE SET \
                            bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20)), \
                            touch = CURRENT_DATE \
                         RETURNING src_id, dst_id",
                    )
                    .param(from)
                    .param(to)
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT dst_id AS id FROM friend_edges WHERE src_id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_1_with_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT fe.dst_id AS id FROM friend_edges fe \
                         JOIN users u ON u.id = fe.dst_id \
                         WHERE fe.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_2", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe2.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         WHERE fe1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_2_with_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe2.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN users u ON u.id = fe2.dst_id \
                         WHERE fe1.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_3", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe3.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id \
                         WHERE fe1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_3_with_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe3.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id \
                         JOIN users u ON u.id = fe3.dst_id \
                         WHERE fe1.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_4", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe4.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id \
                         JOIN friend_edges fe4 ON fe4.src_id = fe3.dst_id \
                         WHERE fe1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_4_with_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT DISTINCT fe4.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN friend_edges fe3 ON fe3.src_id = fe2.dst_id \
                         JOIN friend_edges fe4 ON fe4.src_id = fe3.dst_id \
                         JOIN users u ON u.id = fe4.dst_id \
                         WHERE fe1.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("aggregate_age", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT avg(age) AS avg_age FROM users")
                    .build()
            })
            .add_query("aggregate_age_distinct", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT count(DISTINCT age) AS distinct_ages FROM users")
                    .build()
            })
            .add_query("aggregate_age_filtered", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT avg(age) AS avg_age FROM users WHERE age >= 18")
                    .build()
            })
            .add_query("aggregate_count_users", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT count(*) AS cnt FROM users")
                    .build()
            })
            .add_query("aggregate_age_min_max_avg", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT min(age) AS min_age, max(age) AS max_age, avg(age) AS avg_age FROM users")
                    .build()
            })
            .add_query("neighbours_2", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT fe2.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         WHERE fe1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT fe2.dst_id AS id FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN users u ON u.id = fe2.dst_id \
                         WHERE fe1.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_data", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT u.* FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN users u ON u.id = fe2.dst_id \
                         WHERE fe1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2_with_data_and_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT u.* FROM friend_edges fe1 \
                         JOIN friend_edges fe2 ON fe2.src_id = fe1.dst_id \
                         JOIN users u ON u.id = fe2.dst_id \
                         WHERE fe1.src_id = $1 AND u.age >= 18",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            // Postgres-only: no native shortest-path primitive, approximated via a bounded
            // recursive CTE. `UNION` (not `UNION ALL`) dedupes (id, depth) pairs so the frontier
            // doesn't grow combinatorially across reconverging paths.
            .add_query("shortest_path", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                SqlQueryBuilder::new()
                    .text(
                        "WITH RECURSIVE bfs(id, depth) AS ( \
                            SELECT $1::int, 0 \
                            UNION \
                            SELECT fe.dst_id, bfs.depth + 1 \
                            FROM bfs JOIN friend_edges fe ON fe.src_id = bfs.id \
                            WHERE bfs.depth < 15 \
                         ) \
                         SELECT min(depth) AS length FROM bfs WHERE id = $2",
                    )
                    .param(from)
                    .param(to)
                    .build()
            })
            .add_query("shortest_path_with_filter", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                SqlQueryBuilder::new()
                    .text(
                        "WITH RECURSIVE bfs(id, depth) AS ( \
                            SELECT $1::int, 0 \
                            UNION \
                            SELECT fe.dst_id, bfs.depth + 1 \
                            FROM bfs JOIN friend_edges fe ON fe.src_id = bfs.id \
                            WHERE bfs.depth < 15 \
                         ) \
                         SELECT min(depth) AS length FROM bfs WHERE id = $2 HAVING min(depth) > 0",
                    )
                    .param(from)
                    .param(to)
                    .build()
            })
            // Postgres-only: `$graphLookup`-style path enumeration isn't available on Mongo, so
            // this family is Postgres-only for parity purposes (see plan doc capability matrix).
            .add_query("pattern_cycle", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT e1.src_id AS a_id, e1.dst_id AS b_id, e2.dst_id AS c_id \
                         FROM friend_edges e1 \
                         JOIN friend_edges e2 ON e2.src_id = e1.dst_id \
                         JOIN friend_edges e3 ON e3.src_id = e2.dst_id AND e3.dst_id = e1.src_id \
                         WHERE e1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("pattern_long", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT $1::int AS a_id, e4.dst_id AS b_id \
                         FROM friend_edges e1 \
                         JOIN friend_edges e2 ON e2.src_id = e1.dst_id \
                         JOIN friend_edges e3 ON e3.src_id = e2.dst_id \
                         JOIN friend_edges e4 ON e4.src_id = e3.dst_id \
                         WHERE e1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("pattern_short", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT $1::int AS a_id, e2.dst_id AS b_id \
                         FROM friend_edges e1 \
                         JOIN friend_edges e2 ON e2.src_id = e1.dst_id \
                         WHERE e1.src_id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_label_property", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT * FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_label_property_index", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT * FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("vertex_on_property", QueryType::Read, |random| {
                // Postgres has no label concept; this is equivalent to a plain property lookup.
                SqlQueryBuilder::new()
                    .text("SELECT * FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("value_join", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT b.id FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("value_join_cnt", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT count(b.id) AS cnt FROM users a JOIN users b ON a.age = b.age WHERE a.id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("order_by_age", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT id, age FROM users ORDER BY age, id")
                    .build()
            })
            .add_query("unwind_rows", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT x FROM users u \
                         CROSS JOIN LATERAL (VALUES (u.id), (u.id + 1), (u.id + 2)) AS t(x) \
                         WHERE u.id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("var_len_friends", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "WITH RECURSIVE vlf(id, depth) AS ( \
                            SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 \
                            UNION \
                            SELECT fe.dst_id, vlf.depth + 1 \
                            FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id \
                            WHERE vlf.depth < 2 \
                         ) \
                         SELECT DISTINCT id FROM vlf",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("optional_friend", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT t.a AS a_id, fe.dst_id AS b_id \
                         FROM (SELECT $1::int AS a) t \
                         LEFT JOIN friend_edges fe ON fe.src_id = t.a",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("call_subquery", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT sub.bid FROM (SELECT $1::int AS a) t, \
                         LATERAL (SELECT dst_id AS bid FROM friend_edges WHERE src_id = t.a) sub",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("id_seek", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text("SELECT id FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("id_range_scan", QueryType::Read, |random| {
                let start = random.random_vertex();
                SqlQueryBuilder::new()
                    .text("SELECT id FROM users WHERE id >= $1 AND id < $2")
                    .param(start)
                    .param(start + 100)
                    .build()
            })
            .add_query("merge_user_insert_path", QueryType::Write, |random| {
                let insert_id = random.vertices.saturating_add(random.random_vertex());
                SqlQueryBuilder::new()
                    .text(
                        "INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2) \
                         ON CONFLICT (id) DO NOTHING RETURNING id",
                    )
                    .param(insert_id)
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("merge_user_upsert_existing", QueryType::Write, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "INSERT INTO users (id, created_at, age) VALUES ($1, now(), $2) \
                         ON CONFLICT (id) DO UPDATE SET age = EXCLUDED.age, last_seen = now() \
                         RETURNING id",
                    )
                    .param(random.random_vertex())
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("merge_friend_edge_upsert", QueryType::Write, |random| {
                let (from, to) = random.random_path();
                SqlQueryBuilder::new()
                    .text(
                        "INSERT INTO friend_edges (src_id, dst_id, since, bench_capacity) \
                         VALUES ($1, $2, CURRENT_DATE, 1 + (($1 * 31 + $2 * 17) % 20)) \
                         ON CONFLICT (src_id, dst_id) DO UPDATE SET \
                            touch = CURRENT_DATE, \
                            bench_capacity = COALESCE(friend_edges.bench_capacity, 1 + (($1 * 31 + $2 * 17) % 20)) \
                         RETURNING src_id, dst_id",
                    )
                    .param(from)
                    .param(to)
                    .build()
            })
            .add_query("detach_delete_user", QueryType::Write, |random| {
                // `friend_edges` has ON DELETE CASCADE on both FKs, matching DETACH DELETE semantics.
                SqlQueryBuilder::new()
                    .text("DELETE FROM users WHERE id = $1")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("remove_user_property_and_label", QueryType::Write, |random| {
                // Postgres has no label concept; this drops only the property-removal semantics.
                SqlQueryBuilder::new()
                    .text("UPDATE users SET rpc_social_credit = NULL WHERE id = $1 RETURNING id")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("foreach_loop_mutation", QueryType::Write, |random| {
                // Approximated as a single terminal assignment (equivalent end state to the
                // Cypher `FOREACH (x IN [1,2,3] | SET u.loop_counter = x)`).
                SqlQueryBuilder::new()
                    .text("UPDATE users SET loop_counter = 3 WHERE id = $1 RETURNING loop_counter")
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("union_all_ids", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT id AS uid FROM users WHERE id = $1 \
                         UNION ALL SELECT id AS uid FROM users WHERE id < 10",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            .add_query("union_distinct_ids", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "SELECT id AS uid FROM users WHERE id = $1 \
                         UNION SELECT id AS uid FROM users WHERE id = $1",
                    )
                    .param(random.random_vertex())
                    .build()
            })
            // Postgres-only: bounded (depth <= 4) path-array recursive CTE with explicit
            // cycle-avoidance (`NOT (dst_id = ANY(path))`), approximating `allShortestPaths`.
            .add_query("all_shortest_paths_len", QueryType::Read, |random| {
                let (from, to) = random.random_path();
                SqlQueryBuilder::new()
                    .text(
                        "WITH RECURSIVE paths(id, depth, path) AS ( \
                            SELECT $1::int, 0, ARRAY[$1::int] \
                            UNION ALL \
                            SELECT fe.dst_id, p.depth + 1, p.path || fe.dst_id \
                            FROM paths p JOIN friend_edges fe ON fe.src_id = p.id \
                            WHERE p.depth < 4 AND NOT (fe.dst_id = ANY(p.path)) \
                         ) \
                         SELECT min(depth) AS length FROM paths WHERE id = $2",
                    )
                    .param(from)
                    .param(to)
                    .build()
            })
            .add_query("var_len_with_edge_where_filter", QueryType::Read, |random| {
                SqlQueryBuilder::new()
                    .text(
                        "WITH RECURSIVE vlf(id, depth) AS ( \
                            SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 AND bench_capacity >= $2 \
                            UNION \
                            SELECT fe.dst_id, vlf.depth + 1 \
                            FROM vlf JOIN friend_edges fe ON fe.src_id = vlf.id \
                            WHERE vlf.depth < 3 AND fe.bench_capacity >= $2 \
                         ) \
                         SELECT count(DISTINCT id) AS cnt FROM vlf",
                    )
                    .param(random.random_vertex())
                    .param(1)
                    .build()
            })
            .add_query("count_users_plain", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT count(*) AS cnt FROM users")
                    .build()
            })
            .add_query("count_friend_edges_plain", QueryType::Read, |_random| {
                SqlQueryBuilder::new()
                    .text("SELECT count(*) AS cnt FROM friend_edges")
                    .build()
            })
            .add_query("indexed_or_predicate", QueryType::Read, |random| {
                let (id1, id2) = random.random_path();
                SqlQueryBuilder::new()
                    .text("SELECT id FROM users WHERE id = $1 OR id = $2")
                    .param(id1)
                    .param(id2)
                    .build()
            })
            .add_query("indexed_in_list_predicate", QueryType::Read, |random| {
                let id1 = random.random_vertex();
                let id2 = random.random_vertex();
                let id3 = random.random_vertex();
                let id4 = random.random_vertex();
                SqlQueryBuilder::new()
                    .text("SELECT id FROM users WHERE id IN ($1, $2, $3, $4)")
                    .param(id1)
                    .param(id2)
                    .param(id3)
                    .param(id4)
                    .build()
            });

        if query_coverage_profile.includes_extended_core() {
            builder = builder
                .add_query("exact_5_hop_traverse_count", QueryType::Read, |random| {
                    SqlQueryBuilder::new()
                        .text(
                            "WITH RECURSIVE hops(id, depth) AS ( \
                                SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 \
                                UNION \
                                SELECT fe.dst_id, hops.depth + 1 \
                                FROM hops JOIN friend_edges fe ON fe.src_id = hops.id \
                                WHERE hops.depth < 5 \
                             ) \
                             SELECT count(*) AS cnt FROM hops WHERE depth = 5",
                        )
                        .param(random.random_vertex())
                        .build()
                })
                .add_query("exact_6_hop_traverse_count", QueryType::Read, |random| {
                    SqlQueryBuilder::new()
                        .text(
                            "WITH RECURSIVE hops(id, depth) AS ( \
                                SELECT dst_id, 1 FROM friend_edges WHERE src_id = $1 \
                                UNION \
                                SELECT fe.dst_id, hops.depth + 1 \
                                FROM hops JOIN friend_edges fe ON fe.src_id = hops.id \
                                WHERE hops.depth < 6 \
                             ) \
                             SELECT count(*) AS cnt FROM hops WHERE depth = 6",
                        )
                        .param(random.random_vertex())
                        .build()
                })
                .add_query("temporal_spatial_roundtrip", QueryType::Read, |_random| {
                    // No PostGIS dependency: distance is computed manually via the spherical
                    // law of cosines (haversine-style), matching the plan's stated approach.
                    SqlQueryBuilder::new()
                        .text(
                            "SELECT \
                                DATE '2024-01-01' AS d, \
                                TIME '12:30:00' AS t, \
                                INTERVAL '2 days 3 hours' AS dur, \
                                (6371000 * acos( \
                                    cos(radians(32.1)) * cos(radians(32.2)) * cos(radians(34.9) - radians(34.8)) \
                                    + sin(radians(32.1)) * sin(radians(32.2)) \
                                )) AS dist",
                        )
                        .build()
                });
        }

        let repo = builder.build();
        PostgresUsersQueriesRepository { repo }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_catalog_excludes_unsupported_families() {
        let repo = PostgresUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::Baseline);
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(!names.contains(&"algo_pagerank_summary".to_string()));
        assert!(!names.contains(&"vector_query_nodes_smoke".to_string()));
        assert!(!names.contains(&"entity_path_introspection".to_string()));
        assert!(names.contains(&"shortest_path".to_string()));
        assert!(names.contains(&"pattern_cycle".to_string()));
        assert!(!names.contains(&"exact_5_hop_traverse_count".to_string()));
    }

    #[test]
    fn extended_core_adds_extra_queries() {
        let repo =
            PostgresUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::ExtendedCore);
        let names: Vec<String> = repo.catalog().into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"exact_5_hop_traverse_count".to_string()));
        assert!(names.contains(&"exact_6_hop_traverse_count".to_string()));
        assert!(names.contains(&"temporal_spatial_roundtrip".to_string()));
    }

    #[test]
    fn random_query_generation_produces_bound_params() {
        let repo = PostgresUsersQueriesRepository::new(1000, 10000, QueryCoverageProfile::Baseline);
        for _ in 0..50 {
            let q = repo.random_query(0.5).expect("expected a query");
            assert!(!q.sql.text.is_empty());
        }
    }
}
