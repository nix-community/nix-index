use std::path::PathBuf;

use crate::package::StorePath;

error_chain::error_chain! {
    errors {
        QueryPackages(set: Option<String>) {
            description("query packages error")
            display("querying available packages in set '{}' failed", set.as_ref().unwrap_or(&".".to_string()))
        }
        FetchFiles(path: StorePath) {
            description("file listing fetch error")
            display("fetching the file listing for store path '{}' failed", path.as_str())
        }
        FetchReferences(path: StorePath) {
            description("references fetch error")
            display("fetching the references of store path '{}' failed", path.as_str())
        }
        LoadPathsCache {
            description("paths.cache load error")
            display("loading the paths.cache file failed")
        }
        WritePathsCache {
            description("paths.cache write error")
            display("writing the paths.cache file failed")
        }
        CreateDatabase(path: PathBuf) {
            description("crate database error")
            display("creating the database at '{}' failed", path.to_string_lossy())
        }
        CreateDatabaseDir(path: PathBuf) {
            description("crate database directory error")
            display("creating the directory for the database at '{}' failed", path.to_string_lossy())
        }
        WriteDatabase(path: PathBuf) {
            description("database write error")
            display("writing to the database '{}' failed", path.to_string_lossy())
        }
        ParseProxy(err: crate::hydra::Error){
            description("proxy parse error")
            display("Can not parse proxy settings")
        }
    }
}
