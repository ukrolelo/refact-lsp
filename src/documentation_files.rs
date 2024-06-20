use std::sync::Arc;
use crate::global_context::GlobalContext;
use tokio::sync::{RwLock as ARwLock};
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;
use itertools::Itertools;
use log::{error, info, warn};
use crate::at_tools::att_doc_sources::DocOrigin;
use crate::files_in_workspace::Document;

pub async fn enqueue_all_documentation_files(gcx: Arc<ARwLock<GlobalContext>>) {
    let Ok(paths) = fs::read_dir("./.refact/docs") else {
        warn!("No ./.refact/docs directory");
        return;
    };

    for path in paths {
        let Ok(path) = path else {
            continue;
        };


        let mut path = path.path();
        path.push("origin.json");
        let Ok(file) = fs::File::open(path.clone()) else {
            continue;
        };

        let reader = BufReader::new(file);
        let Some(doc_origin): Option<DocOrigin> = serde_json::from_reader(reader).ok() else {
            error!("Unable to parse {}", path.display());
            continue;
        };

        let documents = doc_origin.pages.values().map(|file_path| Document::new(&PathBuf::from(file_path))).collect_vec();
        let gcx = gcx.write().await;
        let vec_db_module = {
            *gcx.documents_state.cache_dirty.lock().await = true;
            gcx.vec_db.clone()
        };
        match *vec_db_module.lock().await {
            Some(ref mut db) => db.vectorizer_enqueue_files(&documents, false).await,
            None => {}
        };
        let mut sources = gcx.documents_state.documentation_sources.lock().await;
        if !sources.contains(&doc_origin.url) {
            sources.push(doc_origin.url.clone());
        }
    }
}