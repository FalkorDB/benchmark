use crate::queries_repository::{QueryCatalogEntry, QueryCoverageProfile, QueryType};
use crate::spanner_query::{PreparedSpannerQuery, SpannerStatement, SpannerStatementBuilder};
use rand::prelude::IndexedRandom;
use std::collections::HashMap;

type SpannerQueryFn = Box<dyn Fn() -> SpannerStatement + Send + Sync>;
type SpannerQueryEntry = (String, QueryType, SpannerQueryFn);

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

struct SpannerQueryGenerator {
    query_type: QueryType,
    generator: SpannerQueryFn,
}

impl SpannerQueryGenerator {
    fn generate(&self) -> SpannerStatement {
        (self.generator)()
    }
}

pub struct SpannerQueriesRepository {
    read_queries: HashMap<String, SpannerQueryGenerator>,
    write_queries: HashMap<String, SpannerQueryGenerator>,
    read_query_names: Vec<String>,
    write_query_names: Vec<String>,
    name_to_id: HashMap<String, u16>,
    catalog: Vec<QueryCatalogEntry>,
}

impl SpannerQueriesRepository {
    fn new() -> Self {
        SpannerQueriesRepository {
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
        generator: SpannerQueryFn,
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
                    .insert(name, SpannerQueryGenerator { query_type, generator });
            }
            QueryType::Write => {
                self.write_query_names.push(name.clone());
                self.write_queries
                    .insert(name, SpannerQueryGenerator { query_type, generator });
            }
        }
    }

    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.catalog.clone()
    }

    fn random_query_from_pool(
        &self,
        queries: &HashMap<String, SpannerQueryGenerator>,
        query_names: &[String],
    ) -> Option<PreparedSpannerQuery> {
        let mut rng = rand::rng();
        let key = query_names.choose(&mut rng)?;
        let generator = queries.get(key)?;
        let q_id = *self.name_to_id.get(key).unwrap_or(&0);
        Some(PreparedSpannerQuery::new(
            q_id,
            key.clone(),
            generator.query_type,
            generator.generate(),
        ))
    }

    pub fn random_query(
        &self,
        query_type: QueryType,
    ) -> Option<PreparedSpannerQuery> {
        let (queries, query_names) = match query_type {
            QueryType::Read => (&self.read_queries, &self.read_query_names),
            QueryType::Write => (&self.write_queries, &self.write_query_names),
        };
        self.random_query_from_pool(queries, query_names)
    }
}

struct SpannerQueriesRepositoryBuilder {
    vertices: i32,
    graph_name: String,
    queries: Vec<SpannerQueryEntry>,
}

impl SpannerQueriesRepositoryBuilder {
    fn new(
        vertices: i32,
        graph_name: String,
    ) -> Self {
        SpannerQueriesRepositoryBuilder {
            vertices,
            graph_name,
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
        F: Fn(&RandomUtil, &str) -> SpannerStatement + Send + Sync + 'static,
    {
        let vertices = self.vertices;
        let graph_name = self.graph_name.clone();
        self.queries.push((
            name.into(),
            query_type,
            Box::new(move || {
                let random = RandomUtil { vertices };
                generator(&random, &graph_name)
            }),
        ));
        self
    }

    fn build(self) -> SpannerQueriesRepository {
        let mut repo = SpannerQueriesRepository::new();
        for (idx, (name, query_type, generator)) in self.queries.into_iter().enumerate() {
            repo.add_with_id(idx as u16, name, query_type, generator);
        }
        repo
    }
}

pub struct SpannerUsersQueriesRepository {
    repo: SpannerQueriesRepository,
}

impl SpannerUsersQueriesRepository {
    pub fn catalog(&self) -> Vec<QueryCatalogEntry> {
        self.repo.catalog()
    }

    pub fn random_queries(
        self,
        count: usize,
        write_ratio: f32,
    ) -> Box<dyn Iterator<Item = PreparedSpannerQuery> + Send + Sync> {
        Box::new((0..count).filter_map(move |_| self.random_query(write_ratio)))
    }

    pub fn random_query(
        &self,
        write_ratio: f32,
    ) -> Option<PreparedSpannerQuery> {
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

    /// Build the Spanner Graph baseline catalog.
    ///
    /// Reads use GQL (`GRAPH ... MATCH ...`). Writes use GoogleSQL DML against the underlying
    /// `Users`/`FriendEdges` tables (GQL is read-oriented for this workload).
    ///
    /// Intentionally omitted (no fair GQL equivalent in this baseline):
    /// - `all_shortest_paths_*` (Spanner supports `ANY SHORTEST`, not all-shortest enumeration)
    /// - algorithm procedures (pageRank/maxFlow/MSF/harmonic)
    /// - vector/fulltext fixture queries
    pub fn new(
        vertices: i32,
        _edges: i32,
        query_coverage_profile: QueryCoverageProfile,
        graph_name: &str,
    ) -> SpannerUsersQueriesRepository {
        let mut builder = SpannerQueriesRepositoryBuilder::new(vertices, graph_name.to_string())
            .add_query("single_vertex_read", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (u:User {{id: @id}})\n\
                         RETURN u.id AS id, u.age AS age, u.gender AS gender"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_write", QueryType::Write, |random, _graph| {
                // Insert-or-ignore style via INSERT OR IGNORE isn't in Spanner; use INSERT then
                // tolerate already-exists at execution, or INSERT OR UPDATE.
                SpannerStatementBuilder::new()
                    .text(
                        "INSERT OR UPDATE Users (id, completion_percentage, gender, age) \
                         VALUES (@id, 0, 'unknown', 0)",
                    )
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_update", QueryType::Write, |random, _graph| {
                SpannerStatementBuilder::new()
                    .text("UPDATE Users SET age = age + 1 WHERE id = @id")
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("single_vertex_delete", QueryType::Write, |random, _graph| {
                // Avoid cascading mass deletes of real data: delete a synthetic high id.
                let id = random.random_vertex() as i64 + 2_000_000_000;
                SpannerStatementBuilder::new()
                    .text("DELETE FROM Users WHERE id = @id")
                    .param("id", id as i32)
                    .build()
            })
            .add_query("aggregate_expansion_1", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->(b:User)\n\
                         RETURN count(*) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_2", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->()-[:Friend]->(b:User)\n\
                         RETURN count(*) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_3", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->{{1,3}}(b:User)\n\
                         RETURN count(DISTINCT b.id) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("aggregate_expansion_4", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->{{1,4}}(b:User)\n\
                         RETURN count(DISTINCT b.id) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("expand_1_property", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->(b:User)\n\
                         RETURN b.id AS id, b.age AS age"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("expand_1_conditional", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->(b:User)\n\
                         WHERE b.age > 25\n\
                         RETURN b.id AS id"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("neighbours_2", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->{{2,2}}(b:User)\n\
                         RETURN DISTINCT b.id AS id"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("property_filter", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (u:User)\n\
                         WHERE u.age = @age\n\
                         RETURN u.id AS id\n\
                         LIMIT 100"
                    ))
                    .param("age", (random.random_vertex() % 80) + 1)
                    .build()
            })
            .add_query("edge_write", QueryType::Write, |random, _graph| {
                let (src, dst) = random.random_path();
                SpannerStatementBuilder::new()
                    .text(
                        "INSERT OR UPDATE FriendEdges (src_id, dst_id, bench_capacity) \
                         VALUES (@src, @dst, 1)",
                    )
                    .param("src", src)
                    .param("dst", dst)
                    .build()
            })
            // ANY SHORTEST only — all-shortest is intentionally unsupported / omitted.
            .add_query("shortest_path", QueryType::Read, |random, graph| {
                let (src, dst) = random.random_path();
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH p = ANY SHORTEST \
                         (a:User {{id: @src}})-[:Friend]->{{1,10}}(b:User {{id: @dst}})\n\
                         RETURN path_length(p) AS len"
                    ))
                    .param("src", src)
                    .param("dst", dst)
                    .build()
            })
            .add_query("pattern_short", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->(b:User)-[:Friend]->(c:User)\n\
                         RETURN count(*) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_cycle", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->(b:User)-[:Friend]->(a)\n\
                         RETURN count(*) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("pattern_long", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[:Friend]->{{1,4}}(b:User)\n\
                         RETURN count(DISTINCT b.id) AS c"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("inbound_neighbours", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User)-[:Friend]->(b:User {{id: @id}})\n\
                         RETURN a.id AS id"
                    ))
                    .param("id", random.random_vertex())
                    .build()
            })
            .add_query("edge_capacity_filter", QueryType::Read, |random, graph| {
                SpannerStatementBuilder::new()
                    .text(format!(
                        "GRAPH {graph}\n\
                         MATCH (a:User {{id: @id}})-[e:Friend]->(b:User)\n\
                         WHERE e.bench_capacity >= @cap\n\
                         RETURN b.id AS id, e.bench_capacity AS cap"
                    ))
                    .param("id", random.random_vertex())
                    .param("cap", 1)
                    .build()
            });

        if matches!(
            query_coverage_profile,
            QueryCoverageProfile::ExtendedCore | QueryCoverageProfile::FixtureDependent
        ) {
            builder = builder
                .add_query("exact_5_hop_traverse_count", QueryType::Read, |random, graph| {
                    SpannerStatementBuilder::new()
                        .text(format!(
                            "GRAPH {graph}\n\
                             MATCH (a:User {{id: @id}})-[:Friend]->{{5,5}}(b:User)\n\
                             RETURN count(*) AS c"
                        ))
                        .param("id", random.random_vertex())
                        .build()
                })
                .add_query("exact_6_hop_traverse_count", QueryType::Read, |random, graph| {
                    SpannerStatementBuilder::new()
                        .text(format!(
                            "GRAPH {graph}\n\
                             MATCH (a:User {{id: @id}})-[:Friend]->{{6,6}}(b:User)\n\
                             RETURN count(*) AS c"
                        ))
                        .param("id", random.random_vertex())
                        .build()
                });
        }

        SpannerUsersQueriesRepository {
            repo: builder.build(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_omits_all_shortest_and_algorithms() {
        let repo = SpannerUsersQueriesRepository::new(
            1000,
            1000,
            QueryCoverageProfile::Baseline,
            "PokecGraph",
        );
        let names: Vec<_> = repo.catalog().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"shortest_path".to_string()));
        assert!(names.contains(&"single_vertex_read".to_string()));
        assert!(!names.iter().any(|n| n.contains("all_shortest")));
        assert!(!names.iter().any(|n| n.starts_with("algo_")));
        assert!(!names.iter().any(|n| n.contains("vector")));
    }
}
