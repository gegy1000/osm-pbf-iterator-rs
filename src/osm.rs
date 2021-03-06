use ::PbfParseError;
use blob::{Blob, BlobType};
use protos::osm::{DenseNodes, HeaderBlock, Info, Node, PrimitiveBlock, PrimitiveGroup, Relation, Relation_MemberType, StringTable, Way};
use reader::BlobReader;
use std::io::{Read, Seek};
use std::str;
use visitor::{BlobVisitor, OsmVisitor};

pub const NANODEGREE_UNIT: f64 = 1e-9;

pub struct OsmReader<'a, T: 'a + Read + Seek> {
    reader: BlobReader<'a, T>,
}

impl<'a, T: 'a + Read + Seek> OsmReader<'a, T> {
    pub fn from(reader: BlobReader<'a, T>) -> OsmReader<'a, T> {
        OsmReader { reader }
    }

    pub fn accept(&mut self, visitor: &mut OsmVisitor) {
        self.reader.accept(&mut OsmBlobVisitor::new(visitor));
    }
}

struct OsmBlobVisitor<'a> {
    delegate: &'a mut OsmVisitor,
}

impl<'a> OsmBlobVisitor<'a> {
    fn new(delegate: &'a mut OsmVisitor) -> OsmBlobVisitor<'a> {
        OsmBlobVisitor { delegate }
    }

    fn visit_group(&mut self, parser: &OsmBlockParser, group: &PrimitiveGroup) -> Result<(), PbfParseError> {
        self.delegate.visit_group()?;
        let nodes = group.get_nodes();
        let ways = group.get_ways();
        let relations = group.get_relations();
        if !nodes.is_empty() {
            self.visit_nodes(parser, nodes)?;
        } else if !ways.is_empty() {
            self.visit_ways(parser, ways)?;
        } else if !relations.is_empty() {
            self.visit_relations(parser, relations)?;
        } else if group.has_dense() {
            self.visit_dense_nodes(parser, group.get_dense())?;
        }
        self.delegate.end_group()?;
        Ok(())
    }

    fn visit_nodes(&mut self, parser: &OsmBlockParser, nodes: &[Node]) -> Result<(), PbfParseError> {
        for node in nodes {
            let latitude = parser.get_lat(node.get_lat());
            let longitude = parser.get_lon(node.get_lon());
            let tags = parser.parse_tags(node.get_keys(), node.get_vals());
            let info = parser.parse_info(node.get_info());
            self.delegate.visit_node(node.get_id(), latitude, longitude, tags, info)?;
        }
        Ok(())
    }

    fn visit_ways(&mut self, parser: &OsmBlockParser, ways: &[Way]) -> Result<(), PbfParseError> {
        for way in ways {
            let tags = parser.parse_tags(way.get_keys(), way.get_vals());

            let mut nodes: Vec<NodeReference> = Vec::with_capacity(way.get_refs().len());
            let mut current_node_id: i64 = 0;
            for off_id in way.get_refs().iter() {
                current_node_id += *off_id;
                nodes.push(NodeReference { id: current_node_id });
            }

            let info = parser.parse_info(way.get_info());
            self.delegate.visit_way(way.get_id(), nodes, tags, info)?;
        }

        Ok(())
    }

    fn visit_relations(&mut self, parser: &OsmBlockParser, relations: &[Relation]) -> Result<(), PbfParseError> {
        for relation in relations {
            let tags = parser.parse_tags(relation.get_keys(), relation.get_vals());

            let types = relation.get_types();
            let roles = relation.get_roles_sid();

            let mut members: Vec<MemberReference> = Vec::with_capacity(relation.get_memids().len());
            let mut current_member_id: i64 = 0;

            for (i, off_id) in relation.get_memids().iter().enumerate() {
                current_member_id += *off_id;
                let entity_type = OsmEntityType::from(types[i]);
                let role_sid = roles[i];
                members.push(MemberReference { id: current_member_id, entity_type, role_sid });
            }

            let info = parser.parse_info(relation.get_info());
            self.delegate.visit_relation(relation.get_id(), members, tags, info)?;
        }

        Ok(())
    }

    fn visit_dense_nodes(&mut self, parser: &OsmBlockParser, dense: &DenseNodes) -> Result<(), PbfParseError> {
        let info = dense.get_denseinfo();

        let mut current_id: i64 = 0;
        let mut current_lat: i64 = 0;
        let mut current_lon: i64 = 0;
        let mut current_timestamp: i64 = 0;
        let mut current_changeset: i64 = 0;
        let mut current_uid: i32 = 0;
        let mut current_user_sid: i32 = 0;

        let ids = dense.get_id();
        let lats = dense.get_lat();
        let lons = dense.get_lon();
        let raw_tags = dense.get_keys_vals();

        let mut raw_tag_index = 0;

        let versions = info.get_version();
        let timestamps = info.get_timestamp();
        let changesets = info.get_changeset();
        let uids = info.get_uid();
        let user_sids = info.get_user_sid();
        let visibility = info.get_visible();

        for i in 0..ids.len() {
            current_id += ids[i];
            current_lat += lats[i];
            current_lon += lons[i];
            current_timestamp += timestamps[i];
            current_changeset += changesets[i];
            current_uid += uids[i];
            current_user_sid += user_sids[i];

            let mut tags: Vec<(String, String)> = Vec::new();
            while raw_tag_index < raw_tags.len() {
                if raw_tags[raw_tag_index] == 0 {
                    raw_tag_index += 1;
                    break;
                }
                // If we haven't reached the end of this node's tags we should always have a key and value available
                if raw_tag_index + 2 > raw_tags.len() {
                    return Err(PbfParseError::MalformedData);
                }
                let key_id = raw_tags[raw_tag_index];
                let val_id = raw_tags[raw_tag_index + 1];
                tags.push((parser.get_string(key_id), parser.get_string(val_id)));
                raw_tag_index += 2;
            }

            let info = EntityInfo {
                version: versions[i],
                timestamp: parser.get_time(current_timestamp),
                changeset: current_changeset,
                uid: current_uid,
                user_sid: current_user_sid as u32,
                // TODO: Check what this actually means and whether default should be true
                visible: if i < visibility.len() { visibility[i] } else { true },
            };

            self.delegate.visit_node(current_id, parser.get_lat(current_lat), parser.get_lon(current_lon), tags, info)?;
        }

        Ok(())
    }
}

impl<'a> BlobVisitor for OsmBlobVisitor<'a> {
    fn visit_blob(&mut self, blob: &Blob) -> Result<(), PbfParseError> {
        let data = blob.data.as_ref();
        match blob.data_type {
            BlobType::DATA => {
                let block: PrimitiveBlock = ::read_message_bytes(data)?;
                let block_parser = OsmBlockParser::new(&block);
                self.delegate.visit_block(block.get_lat_offset(), block.get_lon_offset(), block.get_granularity(), block.get_date_granularity())?;
                self.delegate.visit_string_table(&block_parser.strings)?;
                for group in block.get_primitivegroup() {
                    self.visit_group(&block_parser, group)?;
                }
                self.delegate.end_block()?;
            }
            BlobType::HEADER => {
                let block: HeaderBlock = ::read_message_bytes(data)?;
                self.delegate.visit_header(&block)?;
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), PbfParseError> {
        self.delegate.end()
    }

    fn handle_error(&mut self, error: &PbfParseError) -> bool {
        self.delegate.handle_error(error)
    }
}

struct OsmBlockParser<'a> {
    origin_latitude: i64,
    origin_longitude: i64,
    granularity: i64,
    date_granularity: i64,
    strings: Vec<&'a str>,
}

impl<'a> OsmBlockParser<'a> {
    fn new(block: &'a PrimitiveBlock) -> OsmBlockParser<'a> {
        OsmBlockParser {
            origin_latitude: block.get_lat_offset(),
            origin_longitude: block.get_lon_offset(),
            granularity: block.get_granularity() as i64,
            date_granularity: block.get_date_granularity() as i64,
            strings: parse_string_table(block.get_stringtable()),
        }
    }

    fn get_lat(&self, lat: i64) -> f64 {
        (self.origin_latitude + lat * self.granularity) as f64 * NANODEGREE_UNIT
    }

    fn get_lon(&self, lon: i64) -> f64 {
        (self.origin_longitude + lon * self.granularity) as f64 * NANODEGREE_UNIT
    }

    fn get_time(&self, time: i64) -> i64 {
        time * self.date_granularity
    }

    fn parse_info(&self, raw_info: &Info) -> EntityInfo {
        EntityInfo {
            version: raw_info.get_version(),
            timestamp: self.get_time(raw_info.get_timestamp()),
            changeset: raw_info.get_changeset(),
            uid: raw_info.get_uid(),
            user_sid: raw_info.get_user_sid(),
            visible: if raw_info.has_visible() { raw_info.get_visible() } else { true },
        }
    }

    fn parse_tags(&self, keys: &'a [u32], values: &'a [u32]) -> Vec<(String, String)> {
        keys.iter().zip(values)
            .map(|(key, value)| (
                self.strings[*key as usize].to_string(),
                self.strings[*value as usize].to_string()
            ))
            .collect()
    }

    fn get_string(&self, id: i32) -> String {
        self.strings[id as usize].to_string()
    }
}

fn parse_string_table<'a>(table: &'a StringTable) -> Vec<&'a str> {
    let table_data = table.get_s();
    table_data.iter()
        .map(|s| str::from_utf8(s.as_ref()))
        .filter_map(|s| s.ok())
        .collect()
}

#[derive(Debug, Copy, Clone)]
pub struct NodeReference {
    pub id: i64,
}

#[derive(Debug, Copy, Clone)]
pub struct MemberReference {
    pub id: i64,
    pub entity_type: OsmEntityType,
    pub role_sid: i32,
}

#[derive(Debug, Copy, Clone)]
pub struct EntityInfo {
    pub version: i32,
    pub timestamp: i64,
    pub changeset: i64,
    pub uid: i32,
    pub user_sid: u32,
    pub visible: bool,
}

#[derive(Debug, Copy, Clone)]
pub enum OsmEntityType {
    Node,
    Way,
    Relation,
}

impl From<Relation_MemberType> for OsmEntityType {
    fn from(mem_type: Relation_MemberType) -> Self {
        use protos::osm::Relation_MemberType::*;
        match mem_type {
            NODE => OsmEntityType::Node,
            WAY => OsmEntityType::Way,
            RELATION => OsmEntityType::Relation,
        }
    }
}

impl Into<Relation_MemberType> for OsmEntityType {
    fn into(self) -> Relation_MemberType {
        use protos::osm::Relation_MemberType::*;
        match self {
            OsmEntityType::Node => NODE,
            OsmEntityType::Way => WAY,
            OsmEntityType::Relation => RELATION,
        }
    }
}
