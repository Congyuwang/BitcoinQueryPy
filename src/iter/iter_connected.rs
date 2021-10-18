use std::sync::Arc;
use crate::api::BitcoinDB;
use crate::iter::fetch_connected_async::{connect_outpoints, update_unspent_cache};
#[cfg(not(feature = "on-disk-utxo"))]
use crate::iter::util::VecMap;
#[cfg(not(feature = "on-disk-utxo"))]
use std::sync::Mutex;
use crate::parser::proto::connected_proto::BlockConnectable;
#[cfg(not(feature = "on-disk-utxo"))]
use crate::parser::proto::connected_proto::TxConnectable;
#[cfg(not(feature = "on-disk-utxo"))]
use hash_hasher::HashedMap;
#[cfg(feature = "on-disk-utxo")]
use log::{error, warn};
#[cfg(feature = "on-disk-utxo")]
use rocksdb::{Options, PlainTableFactoryOptions, SliceTransform, WriteOptions, DB};
#[cfg(feature = "on-disk-utxo")]
use tempdir::TempDir;
use crate::iter::iter::ParIter;

const MAX_SIZE_FOR_THREAD: usize = 10;

/// iterate through blocks, and connecting outpoints.
pub struct ConnectedBlockIter<TBlock> {
    inner: ParIter<TBlock>,
    #[cfg(feature = "on-disk-utxo")]
    cache: Option<TempDir>,
}

impl<TBlock> ConnectedBlockIter<TBlock>
where
    TBlock: 'static + BlockConnectable + Send,
{
    /// the worker threads are dispatched in this `new` constructor!
    pub fn new(db: &BitcoinDB, end: usize) -> Self {
        // UTXO cache
        #[cfg(not(feature = "on-disk-utxo"))]
        let unspent: Arc<
            Mutex<HashedMap<u128, Arc<Mutex<VecMap<<TBlock::Tx as TxConnectable>::TOut>>>>>,
        > = Arc::new(Mutex::new(HashedMap::default()));
        #[cfg(feature = "on-disk-utxo")]
        let cache_dir = {
            match TempDir::new("rocks_db") {
                Ok(tempdir) => tempdir,
                Err(e) => {
                    error!("failed to create rocksDB tempdir for UTXO: {}", e);
                    return ConnectedBlockIter::null();
                }
            }
        };
        #[cfg(feature = "on-disk-utxo")]
        let unspent = {
            let mut options = Options::default();
            // create table
            options.create_if_missing(true);
            // config to more jobs
            options.set_max_background_jobs(cpus as i32);
            // configure mem-table to a large value (1 GB)
            options.set_write_buffer_size(0x40000000);
            // configure l0 and l1 size, let them have the same size (4 GB)
            options.set_level_zero_file_num_compaction_trigger(4);
            options.set_max_bytes_for_level_base(0x100000000);
            // 256MB file size
            options.set_target_file_size_base(0x10000000);
            // use a smaller compaction multiplier
            options.set_max_bytes_for_level_multiplier(4.0);
            // use 8-byte prefix (2 ^ 64 is far enough for transaction counts)
            options.set_prefix_extractor(SliceTransform::create_fixed_prefix(8));
            // set to plain-table for better performance
            options.set_plain_table_factory(&PlainTableFactoryOptions {
                // 16 (compressed txid) + 4 (i32 out n)
                user_key_length: 20,
                bloom_bits_per_key: 10,
                hash_table_ratio: 0.75,
                index_sparseness: 16,
            });
            Arc::new(match DB::open(&options, &cache_dir) {
                Ok(db) => db,
                Err(e) => {
                    error!("failed to create temp rocksDB for UTXO: {}", e);
                    return ConnectedBlockIter::null();
                }
            })
        };
        #[cfg(feature = "on-disk-utxo")]
        let write_options = {
            let mut opt = WriteOptions::default();
            opt.disable_wal(true);
            opt
        };
        // all tasks
        let heights = 0..end;
        let db_copy = db.clone();
        let unspent_copy = unspent.clone();
        let blk_reader = ParIter::new(heights, move |height| {
            update_unspent_cache::<TBlock>(
                &unspent_copy,
                #[cfg(feature = "on-disk-utxo")]
                &write_options,
                &db_copy,
                height,
            )
        });

        let output_iterator = ParIter::new(blk_reader, move |blk| {
            connect_outpoints(&unspent, blk)
        });

        ConnectedBlockIter {
            inner: output_iterator,
            // cache dir will be deleted when ConnectedBlockIter is dropped
            #[cfg(feature = "on-disk-utxo")]
            cache: Some(cache_dir)
        }
    }

    #[cfg(feature = "on-disk-utxo")]
    fn null() -> Self {
        ConnectedBlockIter {
            inner: ParIter::new(Vec::new(), |a: usize| {Err(())}),
            #[cfg(feature = "on-disk-utxo")]
            cache: None
        }
    }
}

impl<TBlock> Iterator for ConnectedBlockIter<TBlock> {
    type Item = TBlock;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

#[cfg(test)]
#[cfg(feature = "on-disk-utxo")]
mod test_empty {
    use crate::{ConnectedBlockIter, SConnectedBlock};

    #[test]
    fn test_empty() {
        let mut empty = ConnectedBlockIter::null();
        for _ in 0..100 {
            let b: Option<SConnectedBlock> = empty.next();
            assert!(b.is_none());
        }
    }
}
