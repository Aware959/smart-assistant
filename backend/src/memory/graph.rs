use crate::db::relation as db_relation;
use crate::db::relation::{Entity, Relation};
use crate::db::Database;
use crate::error::Result;

/// 确保实体存在（按名称去重），返回实体 ID。
pub fn ensure_entity(
    db: &Database,
    name: &str,
    entity_type: &str,
    memory_id: Option<&str>,
) -> Result<String> {
    db_relation::ensure_entity(db, name, entity_type, memory_id)
}

/// 更新/新增一条关系（同一 source-target-type 累加权重）。
pub fn upsert_relation(
    db: &Database,
    source_id: &str,
    target_id: &str,
    relation_type: &str,
    weight: f32,
    memory_id: Option<&str>,
) -> Result<()> {
    db_relation::upsert_relation(db, source_id, target_id, relation_type, weight, memory_id)
}

/// 从结构化提取结果落库实体与关系。
pub fn ingest_extraction(
    db: &Database,
    extraction: &crate::extractor::parser::ExtractionResult,
    memory_id: Option<&str>,
) -> Result<()> {
    let mut entity_ids = std::collections::HashMap::new();

    for ent in &extraction.entities {
        let id = ensure_entity(db, &ent.name, &ent.entity_type, memory_id)?;
        entity_ids.insert(ent.name.clone(), id);
    }

    for rel in &extraction.relations {
        if let (Some(src), Some(tgt)) = (
            entity_ids.get(&rel.source).cloned(),
            entity_ids.get(&rel.target).cloned(),
        ) {
            upsert_relation(db, &src, &tgt, &rel.relation, rel.weight, memory_id)?;
        }
    }

    Ok(())
}

pub fn list_entities(db: &Database) -> Result<Vec<Entity>> {
    db_relation::list_entities(db)
}

pub fn list_relations(db: &Database) -> Result<Vec<Relation>> {
    db_relation::list_relations(db)
}

pub fn list_relations_by_type(db: &Database, relation_type: &str) -> Result<Vec<Relation>> {
    db_relation::list_relations_by_type(db, relation_type)
}

pub fn get_entity(db: &Database, id: &str) -> Result<Option<Entity>> {
    db_relation::get_entity(db, id)
}

pub fn remove_relation(db: &Database, id: &str) -> Result<()> {
    db_relation::delete_relation(db, id)
}

pub fn remove_entity(db: &Database, id: &str) -> Result<()> {
    db_relation::delete_entity(db, id)
}

/// 查询某实体的所有关联关系。
pub fn relations_of_entity(db: &Database, entity_id: &str) -> Result<Vec<Relation>> {
    db_relation::list_relations_for_entity(db, entity_id)
}

/// 依据提取出的实体名查询知识图谱，返回可直接注入提示词的
/// `"源 --[关系]--> 目标"` 描述行（只读，不写入任何实体）。
///
/// 控制召回规模：最多取前 `ENTITY_LIMIT` 个实体，每个实体只带
/// 权重最高的前 `RELATION_PER_ENTITY` 条直接关系（防图谱爆炸式全量注入）。
pub fn recall_graph_context(
    db: &Database,
    extraction: &crate::extractor::parser::ExtractionResult,
) -> Result<Vec<String>> {
    let mut lines: Vec<String> = Vec::new();
    for ent in extraction.entities.iter().take(ENTITY_LIMIT) {
        let Some(entity) = db_relation::find_entity_by_name(db, &ent.name)? else {
            continue;
        };
        let rels = db_relation::list_relations_for_entity(db, &entity.id)?
            .into_iter()
            .take(RELATION_PER_ENTITY);
        for rel in rels {
            let src = db_relation::get_entity(db, &rel.source_id)?
                .map(|e| e.name)
                .unwrap_or_else(|| "未知".to_string());
            let tgt = db_relation::get_entity(db, &rel.target_id)?
                .map(|e| e.name)
                .unwrap_or_else(|| "未知".to_string());
            lines.push(format!("{src} --[{rel}]--> {tgt}", rel = rel.relation_type));
        }
    }
    lines.sort();
    lines.dedup();
    lines.truncate(TOTAL_RELATION_LIMIT);
    Ok(lines)
}

/// 单轮图谱召回最多涉及的实体数。
const ENTITY_LIMIT: usize = 10;
/// 每个实体最多带出的关系条数（`list_relations_for_entity` 已按权重降序）。
const RELATION_PER_ENTITY: usize = 3;
/// 图谱召回行数总上限。
const TOTAL_RELATION_LIMIT: usize = 20;