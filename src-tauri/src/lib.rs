use memmap2::Mmap;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::Value;
use tauri::Manager;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: &str = "3";
const TECHNICAL_PREVIEW_BYTES: usize = 280;
const TECHNICAL_DISPLAY_LIMIT: usize = 1_000_000;

#[derive(Debug, Deserialize)]
struct RawConversation {
    title: Option<String>, create_time: Option<f64>, update_time: Option<f64>,
    conversation_id: Option<String>, id: Option<String>, #[serde(default, rename = "mapping")] _mapping: IgnoredAny,
}
#[derive(Debug, Deserialize)]
struct DetailedConversation { title: Option<String>, current_node: Option<String>, #[serde(default)] mapping: HashMap<String, RawNode> }
#[derive(Debug, Deserialize, Clone)]
struct RawNode { parent: Option<String>, #[serde(default)] children: Vec<String>, message: Option<RawMessage> }
#[derive(Debug, Deserialize, Clone)]
struct RawMessage { author: Option<RawAuthor>, content: Option<RawContent>, create_time: Option<f64>, recipient: Option<String>, #[serde(default)] metadata: Value }
#[derive(Debug, Deserialize, Clone)]
struct RawAuthor { role: Option<String>, name: Option<String> }
#[derive(Debug, Deserialize, Clone)]
struct RawContent { #[serde(flatten)] fields: HashMap<String, Value> }

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConversationSummary { id: String, title: String, created_at: Option<f64>, updated_at: Option<f64>, message_count: usize }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibrarySummary { root: String, source_file: String, conversation_count: usize, indexed_at: String, source_bytes: u64, conversations: Vec<ConversationSummary>, index_status: String, index_duration_ms: u128, source_fingerprint: String }
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MediaRef { asset_pointer: String, path: Option<String>, label: String, mime: Option<String>, width: Option<u64>, height: Option<u64>, size_bytes: Option<u64> }
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ToolStep { command_id: String, command: String, output_id: Option<String>, output_preview: Option<String>, output_bytes: usize, language: Option<String> }
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReaderEntry {
    id: String, kind: String, role: String, author_name: Option<String>, created_at: Option<f64>,
    content_type: String, recipient: Option<String>, text: String, preview: String, text_bytes: usize,
    language: Option<String>, branch_count: usize, media: Vec<MediaRef>, tool_steps: Vec<ToolStep>,
}
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TechnicalMessageSummary { id: String, role: String, author_name: Option<String>, content_type: String, created_at: Option<f64>, recipient: Option<String>, text_preview: String, text_bytes: usize }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagePayload { id: String, text: String, truncated: bool }
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct BranchPoint { id: String, child_count: usize }
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConversationView { id: String, title: String, entries: Vec<ReaderEntry>, technical_messages: Vec<TechnicalMessageSummary>, branch_points: Vec<BranchPoint> }
#[derive(Debug, Clone)]
struct SourceFingerprint { size: u64, modified_ns: u64 }

fn conversations_file(root: &Path) -> Result<PathBuf, String> { let path=root.join("conversations.json"); if path.is_file(){Ok(path)}else{Err("No conversations.json was found. Select an extracted ChatGPT export directory.".into())} }
fn modified_ns(metadata:&fs::Metadata)->Result<u64,String>{metadata.modified().map_err(|e|format!("Unable to inspect source timestamp: {e}"))?.duration_since(UNIX_EPOCH).map_err(|e|format!("Invalid source timestamp: {e}")).map(|d|d.as_nanos().min(u64::MAX as u128)as u64)}
fn fingerprint(path:&Path)->Result<SourceFingerprint,String>{let metadata=fs::metadata(path).map_err(|e|format!("Unable to inspect export: {e}"))?;Ok(SourceFingerprint{size:metadata.len(),modified_ns:modified_ns(&metadata)?})}
fn fingerprint_label(fingerprint:&SourceFingerprint)->String{format!("{}:{}",fingerprint.size,fingerprint.modified_ns)}

fn sidecar_database(root:&Path)->Result<Connection,String>{
    let sidecar=root.join(".gpt-export-browser");fs::create_dir_all(&sidecar).map_err(|e|format!("Unable to create sidecar: {e}"))?;
    let connection=Connection::open(sidecar.join("atlas.sqlite3")).map_err(|e|format!("Unable to open local index: {e}"))?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS library_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS conversations (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at REAL, updated_at REAL, json_offset INTEGER, json_length INTEGER, source_present INTEGER NOT NULL DEFAULT 1, index_generation INTEGER NOT NULL DEFAULT 0); CREATE VIRTUAL TABLE IF NOT EXISTS conversation_fts USING fts5(id UNINDEXED, title); CREATE TABLE IF NOT EXISTS media_assets (asset_key TEXT PRIMARY KEY, relative_path TEXT NOT NULL);").map_err(|e|format!("Unable to initialize local index: {e}"))?;
    for statement in ["ALTER TABLE conversations ADD COLUMN json_offset INTEGER","ALTER TABLE conversations ADD COLUMN json_length INTEGER","ALTER TABLE conversations ADD COLUMN source_present INTEGER NOT NULL DEFAULT 1","ALTER TABLE conversations ADD COLUMN index_generation INTEGER NOT NULL DEFAULT 0"] {let _=connection.execute(statement,[]);}
    Ok(connection)
}
fn meta(connection:&Connection,key:&str)->Result<Option<String>,String>{connection.query_row("SELECT value FROM library_meta WHERE key=?1",params![key],|row|row.get(0)).optional().map_err(|e|format!("Unable to read local index metadata: {e}"))}
fn set_meta(transaction:&rusqlite::Transaction<'_>,key:&str,value:&str)->Result<(),String>{transaction.execute("INSERT INTO library_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).map_err(|e|format!("Unable to write local index metadata: {e}"))?;Ok(())}
fn index_is_current(connection:&Connection,source:&SourceFingerprint)->Result<bool,String>{Ok(meta(connection,"schema_version")?.as_deref()==Some(SCHEMA_VERSION)&&meta(connection,"index_complete")?.as_deref()==Some("true")&&meta(connection,"source_fingerprint")?.as_deref()==Some(fingerprint_label(source).as_str()))}

fn conversation_ranges(bytes:&[u8])->Result<Vec<(u64,u64)>,String>{
    let(mut ranges,mut depth,mut in_string,mut escaped,mut start)=(Vec::new(),0i32,false,false,None);
    for(index,byte)in bytes.iter().copied().enumerate(){if in_string{if escaped{escaped=false}else if byte==b'\\'{escaped=true}else if byte==b'"'{in_string=false}continue;}match byte{b'"'=>in_string=true,b'['|b'{'=>{if byte==b'{'&&depth==1{start=Some(index as u64)}depth+=1},b']'|b'}'=>{if byte==b'}'&&depth==2{let begin=start.take().ok_or("Malformed conversation array")?;ranges.push((begin,index as u64+1));}depth-=1;if depth<0{return Err("Malformed conversations.json nesting".into())}},_=>{}}}
    if in_string||depth!=0||ranges.is_empty(){Err("conversations.json is truncated or malformed".into())}else{Ok(ranges)}
}
fn mapped_source(path:&Path)->Result<(File,Mmap),String>{let file=File::open(path).map_err(|e|format!("Unable to open conversations.json: {e}"))?;let map=unsafe{Mmap::map(&file).map_err(|e|format!("Unable to map conversations.json: {e}"))?};Ok((file,map))}
fn summary_from_slice(bytes:&[u8],offset:u64,_length:u64,index:usize)->Result<ConversationSummary,String>{let item:RawConversation=serde_json::from_slice(bytes).map_err(|e|format!("Could not parse conversation at byte {offset}: {e}"))?;Ok(ConversationSummary{id:item.conversation_id.or(item.id).unwrap_or_else(||format!("conversation-{index}")),title:item.title.filter(|t|!t.trim().is_empty()).unwrap_or_else(||"Untitled conversation".into()),created_at:item.create_time,updated_at:item.update_time,message_count:0})}

fn add_asset_keys(keys:&mut HashSet<String>,name:&str){
    keys.insert(name.to_string());
    if let Some(stem)=Path::new(name).file_stem().and_then(|value|value.to_str()){keys.insert(stem.to_string());keys.insert(stem.trim_end_matches("-sanitized").to_string());}
}
fn scan_media_assets(root:&Path)->Result<Vec<(String,String)>,String>{
    fn walk(root:&Path,current:&Path,rows:&mut Vec<(String,String)>)->Result<(),String>{
        for entry in fs::read_dir(current).map_err(|e|format!("Unable to inspect export media: {e}"))?{let entry=entry.map_err(|e|format!("Unable to inspect export media: {e}"))?;let path=entry.path();if path.file_name().and_then(|v|v.to_str())==Some(".gpt-export-browser"){continue;}if path.is_dir(){walk(root,&path,rows)?;continue;}let Some(name)=path.file_name().and_then(|v|v.to_str())else{continue};if !name.starts_with("file")&&!name.starts_with("dalle"){continue;}let relative=path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\',"/");let mut keys=HashSet::new();add_asset_keys(&mut keys,name);for key in keys{rows.push((key,relative.clone()));}}
        Ok(())
    }
    let mut rows=Vec::new();walk(root,root,&mut rows)?;Ok(rows)
}
fn rebuild_index(root:&Path,source_path:&Path,source:&SourceFingerprint)->Result<(),String>{
    let(_file,map)=mapped_source(source_path)?;let ranges=conversation_ranges(&map)?;let media_assets=scan_media_assets(root)?;let mut connection=sidecar_database(root)?;let generation:i64=meta(&connection,"index_generation")?.and_then(|value|value.parse().ok()).unwrap_or(0)+1;let transaction=connection.transaction().map_err(|e|format!("Unable to start index transaction: {e}"))?;
    transaction.execute("UPDATE conversations SET source_present=0",[]).map_err(|e|format!("Unable to prepare index rebuild: {e}"))?;transaction.execute("DELETE FROM conversation_fts",[]).map_err(|e|format!("Unable to reset text index: {e}"))?;transaction.execute("DELETE FROM media_assets",[]).map_err(|e|format!("Unable to reset media index: {e}"))?;
    let mut upsert=transaction.prepare("INSERT INTO conversations(id,title,created_at,updated_at,json_offset,json_length,source_present,index_generation) VALUES(?1,?2,?3,?4,?5,?6,1,?7) ON CONFLICT(id) DO UPDATE SET title=excluded.title,created_at=excluded.created_at,updated_at=excluded.updated_at,json_offset=excluded.json_offset,json_length=excluded.json_length,source_present=1,index_generation=excluded.index_generation").map_err(|e|format!("Unable to prepare conversation index: {e}"))?;
    let mut insert_fts=transaction.prepare("INSERT INTO conversation_fts(id,title) VALUES(?1,?2)").map_err(|e|format!("Unable to prepare text index: {e}"))?;
    for(index,(offset,end))in ranges.iter().copied().enumerate(){let summary=summary_from_slice(&map[offset as usize..end as usize],offset,end-offset,index)?;upsert.execute(params![&summary.id,&summary.title,summary.created_at,summary.updated_at,offset as i64,(end-offset)as i64,generation]).map_err(|e|format!("Unable to write conversation index: {e}"))?;insert_fts.execute(params![&summary.id,&summary.title]).map_err(|e|format!("Unable to refresh text index: {e}"))?;}
    drop(upsert);drop(insert_fts);let mut insert_media=transaction.prepare("INSERT OR REPLACE INTO media_assets(asset_key,relative_path) VALUES(?1,?2)").map_err(|e|format!("Unable to prepare media index: {e}"))?;for(key,path)in media_assets{insert_media.execute(params![key,path]).map_err(|e|format!("Unable to write media index: {e}"))?;}drop(insert_media);
    set_meta(&transaction,"schema_version",SCHEMA_VERSION)?;set_meta(&transaction,"index_complete","true")?;set_meta(&transaction,"source_fingerprint",&fingerprint_label(source))?;set_meta(&transaction,"index_generation",&generation.to_string())?;transaction.commit().map_err(|e|format!("Unable to finalize local index: {e}"))?;
    fs::write(root.join(".gpt-export-browser").join("manifest.json"),format!("{{\"schemaVersion\":3,\"sourceFingerprint\":\"{}\"}}\n",fingerprint_label(source))).map_err(|e|format!("Unable to write library manifest: {e}"))?;Ok(())
}
fn list_conversations(connection:&Connection,query:Option<&str>)->Result<Vec<ConversationSummary>,String>{let sql=if query.unwrap_or("").trim().is_empty(){"SELECT id,title,created_at,updated_at FROM conversations WHERE source_present=1 ORDER BY updated_at DESC LIMIT 250"}else{"SELECT id,title,created_at,updated_at FROM conversations WHERE source_present=1 AND title LIKE '%' || ?1 || '%' ORDER BY updated_at DESC LIMIT 250"};let mut statement=connection.prepare(sql).map_err(|e|format!("Unable to query local index: {e}"))?;let map=|row:&rusqlite::Row<'_>|Ok(ConversationSummary{id:row.get(0)?,title:row.get(1)?,created_at:row.get(2)?,updated_at:row.get(3)?,message_count:0});let rows=if let Some(query)=query{statement.query_map(params![query.trim()],map)}else{statement.query_map([],map)}.map_err(|e|format!("Unable to search local index: {e}"))?;rows.collect::<Result<Vec<_>,_>>().map_err(|e|format!("Unable to read local index: {e}"))}
fn open_library_sync(root:String,force_refresh:bool)->Result<LibrarySummary,String>{let started=Instant::now();let root_path=PathBuf::from(&root);if !root_path.is_dir(){return Err("The selected path is not a directory.".into())}let source_path=conversations_file(&root_path)?;let source=fingerprint(&source_path)?;let connection=sidecar_database(&root_path)?;let current=!force_refresh&&index_is_current(&connection,&source)?;drop(connection);if !current{rebuild_index(&root_path,&source_path,&source)?;}let connection=sidecar_database(&root_path)?;let count:i64=connection.query_row("SELECT COUNT(*) FROM conversations WHERE source_present=1",[],|row|row.get(0)).map_err(|e|format!("Unable to count indexed conversations: {e}"))?;let conversations=list_conversations(&connection,None)?;Ok(LibrarySummary{root,source_file:source_path.to_string_lossy().into_owned(),conversation_count:count as usize,indexed_at:chrono_like_now(),source_bytes:source.size,conversations,index_status:if current{"ready".into()}else{"rebuilt".into()},index_duration_ms:started.elapsed().as_millis(),source_fingerprint:fingerprint_label(&source)})}
#[tauri::command]async fn open_library(app:tauri::AppHandle,root:String,force_refresh:Option<bool>)->Result<LibrarySummary,String>{tauri::async_runtime::spawn_blocking(move||{let summary=open_library_sync(root,force_refresh.unwrap_or(false))?;app.asset_protocol_scope().allow_directory(Path::new(&summary.root),true).map_err(|e|format!("Unable to allow local export media: {e}"))?;Ok(summary)}).await.map_err(|e|format!("Index task failed: {e}"))?}
#[tauri::command]async fn refresh_library(app:tauri::AppHandle,root:String)->Result<LibrarySummary,String>{open_library(app,root,Some(true)).await}
#[tauri::command]fn search_conversations(root:String,query:String)->Result<Vec<ConversationSummary>,String>{list_conversations(&sidecar_database(Path::new(&root))?,Some(&query))}

fn content_type(content:Option<&RawContent>)->String{content.and_then(|content|content.fields.get("content_type")).and_then(Value::as_str).unwrap_or("unknown").to_string()}
fn value_to_text(value:&Value)->String{match value{Value::String(text)=>text.clone(),Value::Array(values)=>values.iter().map(value_to_text).filter(|text|!text.trim().is_empty()).collect::<Vec<_>>().join("\n"),Value::Object(values)=>["text","result","content","summary","thoughts"].iter().find_map(|key|values.get(*key)).map(value_to_text).unwrap_or_default(),_=>String::new()}}
fn node_text(message:&RawMessage)->String{let Some(content)=message.content.as_ref()else{return String::new()};let parts=content.fields.get("parts").map(value_to_text).unwrap_or_default();if !parts.trim().is_empty(){return parts;}["text","result","content","thoughts","summary"].iter().filter_map(|key|content.fields.get(*key)).map(value_to_text).filter(|text|!text.trim().is_empty()).collect::<Vec<_>>().join("\n")}
fn language(message:&RawMessage)->Option<String>{message.content.as_ref().and_then(|content|content.fields.get("language")).and_then(Value::as_str).map(str::to_string)}
fn truncate_utf8(text:&str,max:usize)->(String,bool){if text.len()<=max{return(text.into(),false)}let mut end=max;while !text.is_char_boundary(end){end-=1}(format!("{}…",&text[..end]),true)}
fn detailed_from_index(root:&str,id:&str)->Result<DetailedConversation,String>{let path=conversations_file(Path::new(root))?;let connection=sidecar_database(Path::new(root))?;let row:Option<(i64,i64)>=connection.query_row("SELECT json_offset,json_length FROM conversations WHERE id=?1 AND source_present=1",params![id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|e|format!("Unable to locate conversation: {e}"))?;let(offset,length)=row.ok_or("Conversation is not in the current source. Refresh the library.")?;if offset<0||length<=0{return Err("Conversation index is invalid. Refresh the library.".into())}let mut file=File::open(path).map_err(|e|format!("Unable to open conversation source: {e}"))?;file.seek(SeekFrom::Start(offset as u64)).map_err(|e|format!("Unable to seek conversation source: {e}"))?;let mut bytes=vec![0;length as usize];file.read_exact(&mut bytes).map_err(|e|format!("Unable to read conversation source: {e}"))?;serde_json::from_slice(&bytes).map_err(|e|format!("Could not parse selected conversation: {e}"))}
fn active_nodes(conversation:&DetailedConversation)->Vec<String>{let leaf=conversation.current_node.clone().or_else(||conversation.mapping.iter().filter(|(_,node)|node.children.is_empty()).filter_map(|(id,node)|node.message.as_ref().and_then(|message|message.create_time).map(|time|(id.clone(),time))).max_by(|a,b|a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(id,_)|id));let mut ids=Vec::new();let mut cursor=leaf;while let Some(id)=cursor{let Some(node)=conversation.mapping.get(&id)else{break};ids.push(id.clone());cursor=node.parent.clone();}ids.reverse();ids}
fn role_and_type(message:&RawMessage)->(String,Option<String>,String,bool){let role=message.author.as_ref().and_then(|author|author.role.clone()).unwrap_or_else(||"unknown".into());let author=message.author.as_ref().and_then(|author|author.name.clone());let content_type=content_type(message.content.as_ref());let hidden=message.metadata.get("is_visually_hidden_from_conversation").and_then(Value::as_bool).unwrap_or(false);(role,author,content_type,hidden)}
fn media_key(pointer:&str)->String{pointer.split("://").nth(1).unwrap_or(pointer).split('?').next().unwrap_or(pointer).trim_matches('/').to_string()}
fn mime_for_path(path:&Path)->Option<String>{match path.extension().and_then(|value|value.to_str()).map(|value|value.to_ascii_lowercase()).as_deref(){Some("png")=>Some("image/png".into()),Some("jpg")|Some("jpeg")=>Some("image/jpeg".into()),Some("webp")=>Some("image/webp".into()),Some("gif")=>Some("image/gif".into()),Some("mp4")=>Some("video/mp4".into()),Some("webm")=>Some("video/webm".into()),Some("mp3")=>Some("audio/mpeg".into()),Some("wav")=>Some("audio/wav".into()),_=>None}}
fn collect_media(value:&Value,items:&mut Vec<(String,Option<u64>,Option<u64>,Option<u64>)>){match value{Value::Array(values)=>for value in values{collect_media(value,items)},Value::Object(values)=>{if let Some(pointer)=values.get("asset_pointer").and_then(Value::as_str){items.push((pointer.to_string(),values.get("width").and_then(Value::as_u64),values.get("height").and_then(Value::as_u64),values.get("size_bytes").and_then(Value::as_u64)));}for value in values.values(){collect_media(value,items)}},_=>{}}}
fn message_media(root:&Path,connection:&Connection,message:&RawMessage)->Vec<MediaRef>{let Some(content)=message.content.as_ref()else{return Vec::new()};let mut pointers=Vec::new();for value in content.fields.values(){collect_media(value,&mut pointers)}let mut seen=HashSet::new();pointers.into_iter().filter(|(pointer,..)|seen.insert(pointer.clone())).map(|(pointer,width,height,size_bytes)|{let key=media_key(&pointer);let relative:Option<String>=connection.query_row("SELECT relative_path FROM media_assets WHERE asset_key=?1 OR asset_key LIKE ?1 || '%' ORDER BY length(asset_key) LIMIT 1",params![key],|row|row.get(0)).optional().ok().flatten();let path=relative.map(|relative|root.join(relative));let label=path.as_ref().and_then(|path|path.file_name()).and_then(|name|name.to_str()).unwrap_or(&key).to_string();let mime=path.as_deref().and_then(mime_for_path);MediaRef{asset_pointer:pointer,path:path.map(|path|path.to_string_lossy().into_owned()),label,mime,width,height,size_bytes}}).collect()}
fn entry(id:String,kind:&str,message:&RawMessage,node:&RawNode,text:String,media:Vec<MediaRef>)->ReaderEntry{let(role,author_name,content_type,_)=role_and_type(message);let(preview,_)=truncate_utf8(&text,TECHNICAL_PREVIEW_BYTES);ReaderEntry{id,kind:kind.into(),role,author_name,created_at:message.create_time,content_type,recipient:message.recipient.clone(),text_bytes:text.len(),text,preview,language:language(message),branch_count:node.children.len(),media,tool_steps:Vec::new()}}
fn conversation_view(root:&Path,conversation:DetailedConversation,requested_id:String)->ConversationView{
    let connection=sidecar_database(root).ok();let ids=active_nodes(&conversation);let mut entries=Vec::new();let mut technical_messages=Vec::new();let mut index=0;
    while index<ids.len(){let id=&ids[index];let Some(node)=conversation.mapping.get(id)else{index+=1;continue};let Some(message)=node.message.as_ref()else{index+=1;continue};let(role,author_name,content_type,hidden)=role_and_type(message);let text=node_text(message);let media=connection.as_ref().map(|connection|message_media(root,connection,message)).unwrap_or_default();
        if hidden{if !text.trim().is_empty()||!media.is_empty(){let(preview,_)=truncate_utf8(&text,TECHNICAL_PREVIEW_BYTES);technical_messages.push(TechnicalMessageSummary{id:id.clone(),role,author_name,content_type,created_at:message.create_time,recipient:message.recipient.clone(),text_preview:preview,text_bytes:text.len()});}index+=1;continue;}
        let delegated=role=="assistant"&&message.recipient.as_deref().is_some_and(|recipient|recipient!="all")&&matches!(content_type.as_str(),"text"|"code");
        if delegated{let mut run=entry(id.clone(),"toolRun",message,node,text.clone(),media);let mut step=ToolStep{command_id:id.clone(),command:text,output_id:None,output_preview:None,output_bytes:0,language:run.language.clone().or_else(||message.recipient.clone())};if let Some(next_id)=ids.get(index+1){if let Some(next_node)=conversation.mapping.get(next_id){if let Some(next)=next_node.message.as_ref(){let(next_role,_,next_type,next_hidden)=role_and_type(next);if !next_hidden&&next_role=="tool"&&matches!(next_type.as_str(),"execution_output"|"system_error"){let output=node_text(next);let(preview,_)=truncate_utf8(&output,TECHNICAL_PREVIEW_BYTES);step.output_id=Some(next_id.clone());step.output_preview=Some(preview);step.output_bytes=output.len();index+=1;}}}}run.tool_steps.push(step);entries.push(run);index+=1;continue;}
        let kind=if matches!(role.as_str(),"user"|"assistant")&&matches!(content_type.as_str(),"text"|"multimodal_text"){"message"}else if content_type=="code"{"code"}else{"technical"};if !text.trim().is_empty()||!media.is_empty(){entries.push(entry(id.clone(),kind,message,node,text,media));}index+=1;
    }
    let branch_points=conversation.mapping.iter().filter(|(_,node)|node.children.len()>1).map(|(id,node)|BranchPoint{id:id.clone(),child_count:node.children.len()}).collect();ConversationView{id:requested_id,title:conversation.title.unwrap_or_else(||"Untitled conversation".into()),entries,technical_messages,branch_points}
}
#[tauri::command]async fn read_conversation(root:String,conversation_id:String)->Result<ConversationView,String>{tauri::async_runtime::spawn_blocking(move||{let conversation=detailed_from_index(&root,&conversation_id)?;Ok(conversation_view(Path::new(&root),conversation,conversation_id))}).await.map_err(|e|format!("Conversation task failed: {e}"))?}
#[tauri::command]async fn read_message_payload(root:String,conversation_id:String,entry_id:String)->Result<MessagePayload,String>{tauri::async_runtime::spawn_blocking(move||{let conversation=detailed_from_index(&root,&conversation_id)?;let node=conversation.mapping.get(&entry_id).ok_or("Message was not found.")?;let message=node.message.as_ref().ok_or("Message is empty.")?;let(text,truncated)=truncate_utf8(&node_text(message),TECHNICAL_DISPLAY_LIMIT);Ok(MessagePayload{id:entry_id,text,truncated})}).await.map_err(|e|format!("Message task failed: {e}"))?}
#[tauri::command]async fn read_technical_message(root:String,conversation_id:String,message_id:String)->Result<MessagePayload,String>{read_message_payload(root,conversation_id,message_id).await}
fn chrono_like_now()->String{format!("{}",SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())}
#[cfg_attr(mobile,tauri::mobile_entry_point)]pub fn run(){tauri::Builder::default().plugin(tauri_plugin_dialog::init()).plugin(tauri_plugin_opener::init()).invoke_handler(tauri::generate_handler![open_library,refresh_library,search_conversations,read_conversation,read_message_payload,read_technical_message]).run(tauri::generate_context!()).expect("error while running tauri application");}

#[cfg(test)]mod tests{use super::*;fn fixture()->String{r##"[{"conversation_id":"one","title":"Rich fixture","current_node":"answer","mapping":{"user":{"parent":null,"children":["command"],"message":{"author":{"role":"user"},"create_time":1,"content":{"content_type":"text","parts":["hello"]},"metadata":{}}},"command":{"parent":"user","children":["output"],"message":{"author":{"role":"assistant"},"recipient":"python","create_time":2,"content":{"content_type":"text","parts":["print('hello')"]},"metadata":{}}},"output":{"parent":"command","children":["answer"],"message":{"author":{"role":"tool","name":"python"},"create_time":3,"content":{"content_type":"execution_output","text":"hello"},"metadata":{}}},"answer":{"parent":"output","children":[],"message":{"author":{"role":"assistant"},"create_time":4,"content":{"content_type":"text","parts":["**world** entity[\"software\",\"Atlas\",0]"]},"metadata":{}}}}}]"##.into()}fn root()->PathBuf{std::env::temp_dir().join(format!("gpt-export-browser-test-{}",std::process::id()))}#[test]fn scanner_handles_json_strings(){let data=fixture();assert_eq!(conversation_ranges(data.as_bytes()).unwrap().len(),1);assert!(conversation_ranges(b"[{").is_err());}#[test]fn entries_keep_tool_output(){let path=root();let _=fs::remove_dir_all(&path);fs::create_dir_all(&path).unwrap();fs::write(path.join("conversations.json"),fixture()).unwrap();let root_text=path.to_string_lossy().into_owned();open_library_sync(root_text.clone(),true).unwrap();let view=conversation_view(&path,detailed_from_index(&root_text,"one").unwrap(),"one".into());assert_eq!(view.entries.len(),3);assert_eq!(view.entries[1].kind,"toolRun");assert_eq!(view.entries[1].tool_steps[0].output_preview.as_deref(),Some("hello"));fs::remove_dir_all(path).unwrap();}#[test]fn extracts_text_result_and_thoughts(){let message:RawMessage=serde_json::from_str(r#"{"content":{"content_type":"execution_output","text":"result"}}"#).unwrap();assert_eq!(node_text(&message),"result");let message:RawMessage=serde_json::from_str(r#"{"content":{"content_type":"thoughts","thoughts":[{"content":"thinking"}]}}"#).unwrap();assert_eq!(node_text(&message),"thinking");}#[test]#[ignore]fn validates_configured_real_export(){let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("gpt-export");if !path.join("conversations.json").is_file(){return;}let root_text=path.to_string_lossy().into_owned();open_library_sync(root_text.clone(),false).unwrap();let connection=sidecar_database(&path).unwrap();for title in ["Starfield 美术风格","编辑按钮无反应分析","Cupcakke介绍"]{let id:String=connection.query_row("SELECT id FROM conversations WHERE title=?1",params![title],|row|row.get(0)).unwrap();let view=conversation_view(&path,detailed_from_index(&root_text,&id).unwrap(),id);assert!(!view.entries.is_empty(),"{title}");if title=="编辑按钮无反应分析"{assert!(view.entries.iter().any(|entry|entry.kind=="toolRun"&&entry.tool_steps.iter().any(|step|step.output_id.is_some())));}}}}
