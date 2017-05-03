
use std::u64;
use std::path::{Path, PathBuf};

use env::InfoLogLevel;
use env::Logger;
use listener::EventListener;
use write_buffer_manager::WriteBufferManager;
use rate_limiter::RateLimiter;
use sst_file_manager::SstFileManager;
use statistics::Statistics;
use cache::Cache;
// unused!
// use advanced_options::AdvancedColumnFamilyOptions;
use advanced_options::{CompactionStyle, CompactionPri, CompactionOptionsFIFO, CompressionOptions};
use universal_compaction::CompactionOptionsUniversal;
use compaction_filter::{CompactionFilter, CompactionFilterFactory};
use merge_operator::MergeOperator;
use table::TableFactory;
use comparator::Comparator;
use slice_transform::SliceTransform;
use snapshot::Snapshot;

/// DB contents are stored in a set of blocks, each of which holds a
/// sequence of key,value pairs.  Each block may be compressed before
/// being stored in a file.  The following enum describes which
/// compression method (if any) is used to compress a block.
#[repr(u8)]
pub enum CompressionType {
    /// NOTE: do not change the values of existing entries, as these are
    /// part of the persistent format on disk.
    NoCompression = 0x0,
    SnappyCompression = 0x1,
    ZlibCompression = 0x2,
    BZip2Compression = 0x3,
    LZ4Compression = 0x4,
    LZ4HCCompression = 0x5,
    XpressCompression = 0x6,
    ZSTD = 0x7,

    /// Only use kZSTDNotFinalCompression if you have to use ZSTD lib older than
    /// 0.8.0 or consider a possibility of downgrading the service or copying
    /// the database files to another service running with an older version of
    /// RocksDB that doesn't have kZSTD. Otherwise, you should use kZSTD. We will
    /// eventually remove the option from the public API.
    ZSTDNotFinalCompression = 0x40,

    /// kDisableCompressionOption is used to disable some compression options.
    DisableCompressionOption = 0xff,
}

#[repr(C)]
pub enum WALRecoveryMode {
    /// Original levelDB recovery
    /// We tolerate incomplete record in trailing data on all logs
    /// Use case : This is legacy behavior (default)
    TolerateCorruptedTailRecords = 0x00,
    /// Recover from clean shutdown
    /// We don't expect to find any corruption in the WAL
    /// Use case : This is ideal for unit tests and rare applications that
    /// can require high consistency guarantee
    AbsoluteConsistency = 0x01,
    /// Recover to point-in-time consistency
    /// We stop the WAL playback on discovering WAL inconsistency
    /// Use case : Ideal for systems that have disk controller cache like
    /// hard disk, SSD without super capacitor that store related data
    PointInTimeRecovery = 0x02,
    /// Recovery after a disaster
    /// We ignore any corruption in the WAL and try to salvage as much data as
    /// possible
    /// Use case : Ideal for last ditch effort to recover data or systems that
    /// operate with low grade unrelated data
    SkipAnyCorruptedRecords = 0x03,
}


pub struct DbPath {
    pub path: PathBuf,
    /// Target size of total files under the path, in byte.
    pub target_size: u64,
}

impl DbPath {
    pub fn new<P: AsRef<Path>>(p: P, t: u64) -> DbPath {
        DbPath {
            path: p.as_ref().to_path_buf(),
            target_size: t,
        }
    }
}

impl Default for DbPath {
    fn default() -> Self {
        DbPath::new("", 0)
    }
}

pub struct ColumnFamilyOptions {
    /// -------------------
    /// Parameters that affect behavior

    /// Comparator used to define the order of keys in the table.
    /// Default: a comparator that uses lexicographic byte-wise ordering
    ///
    /// REQUIRES: The client must ensure that the comparator supplied
    /// here has the same name and orders keys *exactly* the same as the
    /// comparator provided to previous open calls on the same DB.
    pub comparator: Option<Comparator>,

    /// REQUIRES: The client must provide a merge operator if Merge operation
    /// needs to be accessed. Calling Merge on a DB without a merge operator
    /// would result in Status::NotSupported. The client must ensure that the
    /// merge operator supplied here has the same name and *exactly* the same
    /// semantics as the merge operator provided to previous open calls on
    /// the same DB. The only exception is reserved for upgrade, where a DB
    /// previously without a merge operator is introduced to Merge operation
    /// for the first time. It's necessary to specify a merge operator when
    /// openning the DB in this case.
    /// Default: nullptr
    pub merge_operator: Option<MergeOperator>,

    /// A single CompactionFilter instance to call into during compaction.
    /// Allows an application to modify/delete a key-value during background
    /// compaction.
    ///
    /// If the client requires a new compaction filter to be used for different
    /// compaction runs, it can specify compaction_filter_factory instead of this
    /// option.  The client should specify only one of the two.
    /// compaction_filter takes precedence over compaction_filter_factory if
    /// client specifies both.
    ///
    /// If multithreaded compaction is being used, the supplied CompactionFilter
    /// instance may be used from different threads concurrently and so should be
    /// thread-safe.
    ///
    /// Default: nullptr
    pub compaction_filter: Option<CompactionFilter>,

    /// This is a factory that provides compaction filter objects which allow
    /// an application to modify/delete a key-value during background compaction.
    ///
    /// A new filter will be created on each compaction run.  If multithreaded
    /// compaction is being used, each created CompactionFilter will only be used
    /// from a single thread and so does not need to be thread-safe.
    ///
    /// Default: nullptr
    pub compaction_filter_factory: Option<CompactionFilterFactory>,

    /// -------------------
    /// Parameters that affect performance

    /// Amount of data to build up in memory (backed by an unsorted log
    /// on disk) before converting to a sorted on-disk file.
    ///
    /// Larger values increase performance, especially during bulk loads.
    /// Up to max_write_buffer_number write buffers may be held in memory
    /// at the same time,
    /// so you may wish to adjust this parameter to control memory usage.
    /// Also, a larger write buffer will result in a longer recovery time
    /// the next time the database is opened.
    ///
    /// Note that write_buffer_size is enforced per column family.
    /// See db_write_buffer_size for sharing memory across column families.
    ///
    /// Default: 64MB
    ///
    /// Dynamically changeable through SetOptions() API
    pub write_buffer_size: usize,

    /// The maximum number of write buffers that are built up in memory.
    /// The default and the minimum number is 2, so that when 1 write buffer
    /// is being flushed to storage, new writes can continue to the other
    /// write buffer.
    /// If max_write_buffer_number > 3, writing will be slowed down to
    /// options.delayed_write_rate if we are writing to the last write buffer
    /// allowed.
    ///
    /// Default: 2
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_write_buffer_number: i32,

    /// The minimum number of write buffers that will be merged together
    /// before writing to storage.  If set to 1, then
    /// all write buffers are flushed to L0 as individual files and this increases
    /// read amplification because a get request has to check in all of these
    /// files. Also, an in-memory merge may result in writing lesser
    /// data to storage if there are duplicate records in each of these
    /// individual write buffers.  Default: 1
    pub min_write_buffer_number_to_merge: i32,

    /// The total maximum number of write buffers to maintain in memory including
    /// copies of buffers that have already been flushed.  Unlike
    /// max_write_buffer_number, this parameter does not affect flushing.
    /// This controls the minimum amount of write history that will be available
    /// in memory for conflict checking when Transactions are used.
    ///
    /// When using an OptimisticTransactionDB:
    /// If this value is too low, some transactions may fail at commit time due
    /// to not being able to determine whether there were any write conflicts.
    ///
    /// When using a TransactionDB:
    /// If Transaction::SetSnapshot is used, TransactionDB will read either
    /// in-memory write buffers or SST files to do write-conflict checking.
    /// Increasing this value can reduce the number of reads to SST files
    /// done for conflict detection.
    ///
    /// Setting this value to 0 will cause write buffers to be freed immediately
    /// after they are flushed.
    /// If this value is set to -1, 'max_write_buffer_number' will be used.
    ///
    /// Default:
    /// If using a TransactionDB/OptimisticTransactionDB, the default value will
    /// be set to the value of 'max_write_buffer_number' if it is not explicitly
    /// set by the user.  Otherwise, the default is 0.
    pub max_write_buffer_number_to_maintain: i32,

    /// Compress blocks using the specified compression algorithm.  This
    /// parameter can be changed dynamically.
    ///
    /// Default: kSnappyCompression, if it's supported. If snappy is not linked
    /// with the library, the default is kNoCompression.
    ///
    /// Typical speeds of kSnappyCompression on an Intel(R) Core(TM)2 2.4GHz:
    ///    ~200-500MB/s compression
    ///    ~400-800MB/s decompression
    /// Note that these speeds are significantly faster than most
    /// persistent storage speeds, and therefore it is typically never
    /// worth switching to kNoCompression.  Even if the input data is
    /// incompressible, the kSnappyCompression implementation will
    /// efficiently detect that and will switch to uncompressed mode.
    pub compression: CompressionType,

    /// Different levels can have different compression policies. There
    /// are cases where most lower levels would like to use quick compression
    /// algorithms while the higher levels (which have more data) use
    /// compression algorithms that have better compression but could
    /// be slower. This array, if non-empty, should have an entry for
    /// each level of the database; these override the value specified in
    /// the previous field 'compression'.
    ///
    /// NOTICE if level_compaction_dynamic_level_bytes=true,
    /// compression_per_level[0] still determines L0, but other elements
    /// of the array are based on base level (the level L0 files are merged
    /// to), and may not match the level users see from info log for metadata.
    /// If L0 files are merged to level-n, then, for i>0, compression_per_level[i]
    /// determines compaction type for level n+i-1.
    /// For example, if we have three 5 levels, and we determine to merge L0
    /// data to L4 (which means L1..L3 will be empty), then the new files go to
    /// L4 uses compression type compression_per_level[1].
    /// If now L0 is merged to L2. Data goes to L2 will be compressed
    /// according to compression_per_level[1], L3 using compression_per_level[2]
    /// and L4 using compression_per_level[3]. Compaction for each level can
    /// change when data grows.
    pub compression_per_level: Vec<CompressionType>,

    /// Compression algorithm that will be used for the bottommost level that
    /// contain files. If level-compaction is used, this option will only affect
    /// levels after base level.
    ///
    /// Default: kDisableCompressionOption (Disabled)
    pub bottommost_compression: CompressionType,

    /// different options for compression algorithms
    pub compression_opts: CompressionOptions,

    /// If non-nullptr, use the specified function to determine the
    /// prefixes for keys.  These prefixes will be placed in the filter.
    /// Depending on the workload, this can reduce the number of read-IOP
    /// cost for scans when a prefix is passed via ReadOptions to
    /// db.NewIterator().  For prefix filtering to work properly,
    /// "prefix_extractor" and "comparator" must be such that the following
    /// properties hold:
    ///
    /// 1) key.starts_with(prefix(key))
    /// 2) Compare(prefix(key), key) <= 0.
    /// 3) If Compare(k1, k2) <= 0, then Compare(prefix(k1), prefix(k2)) <= 0
    /// 4) prefix(prefix(key)) == prefix(key)
    ///
    /// Default: nullptr
    pub prefix_extractor: Option<SliceTransform>,

    /// Number of levels for this database
    pub num_levels: i32,

    /// Number of files to trigger level-0 compaction. A value <0 means that
    /// level-0 compaction will not be triggered by number of files at all.
    ///
    /// Default: 4
    ///
    /// Dynamically changeable through SetOptions() API
    pub level0_file_num_compaction_trigger: i32,

    /// Soft limit on number of level-0 files. We start slowing down writes at this
    /// point. A value <0 means that no writing slow down will be triggered by
    /// number of files in level-0.
    ///
    /// Default: 20
    ///
    /// Dynamically changeable through SetOptions() API
    pub level0_slowdown_writes_trigger: i32,

    /// Maximum number of level-0 files.  We stop writes at this point.
    ///
    /// Default: 36
    ///
    /// Dynamically changeable through SetOptions() API
    pub level0_stop_writes_trigger: i32,

    /// Target file size for compaction.
    /// target_file_size_base is per-file size for level-1.
    /// Target file size for level L can be calculated by
    /// target_file_size_base * (target_file_size_multiplier ^ (L-1))
    /// For example, if target_file_size_base is 2MB and
    /// target_file_size_multiplier is 10, then each file on level-1 will
    /// be 2MB, and each file on level 2 will be 20MB,
    /// and each file on level-3 will be 200MB.
    ///
    /// Default: 64MB.
    ///
    /// Dynamically changeable through SetOptions() API
    pub target_file_size_base: u64,

    /// By default target_file_size_multiplier is 1, which means
    /// by default files in different levels will have similar size.
    ///
    /// Dynamically changeable through SetOptions() API
    pub target_file_size_multiplier: i32,

    /// Control maximum total data size for a level.
    /// max_bytes_for_level_base is the max total for level-1.
    /// Maximum number of bytes for level L can be calculated as
    /// (max_bytes_for_level_base) * (max_bytes_for_level_multiplier ^ (L-1))
    /// For example, if max_bytes_for_level_base is 200MB, and if
    /// max_bytes_for_level_multiplier is 10, total data size for level-1
    /// will be 200MB, total file size for level-2 will be 2GB,
    /// and total file size for level-3 will be 20GB.
    ///
    /// Default: 256MB.
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_bytes_for_level_base: u64,

    /// If true, RocksDB will pick target size of each level dynamically.
    /// We will pick a base level b >= 1. L0 will be directly merged into level b,
    /// instead of always into level 1. Level 1 to b-1 need to be empty.
    /// We try to pick b and its target size so that
    /// 1. target size is in the range of
    ///   (max_bytes_for_level_base / max_bytes_for_level_multiplier,
    ///    pub max_bytes_for_level_base]
    /// 2. target size of the last level (level num_levels-1) equals to extra size
    ///    pub of the level.
    /// At the same time max_bytes_for_level_multiplier and
    /// max_bytes_for_level_multiplier_additional are still satisfied.
    ///
    /// With this option on, from an empty DB, we make last level the base level,
    /// which means merging L0 data into the last level, until it exceeds
    /// max_bytes_for_level_base. And then we make the second last level to be
    /// base level, to start to merge L0 data to second last level, with its
    /// target size to be 1/max_bytes_for_level_multiplier of the last level's
    /// extra size. After the data accumulates more so that we need to move the
    /// base level to the third last one, and so on.
    ///
    /// For example, assume max_bytes_for_level_multiplier=10, num_levels=6,
    /// and max_bytes_for_level_base=10MB.
    /// Target sizes of level 1 to 5 starts with:
    /// [- - - - 10MB]
    /// with base level is level. Target sizes of level 1 to 4 are not applicable
    /// because they will not be used.
    /// Until the size of Level 5 grows to more than 10MB, say 11MB, we make
    /// base target to level 4 and now the targets looks like:
    /// [- - - 1.1MB 11MB]
    /// While data are accumulated, size targets are tuned based on actual data
    /// of level 5. When level 5 has 50MB of data, the target is like:
    /// [- - - 5MB 50MB]
    /// Until level 5's actual size is more than 100MB, say 101MB. Now if we keep
    /// level 4 to be the base level, its target size needs to be 10.1MB, which
    /// doesn't satisfy the target size range. So now we make level 3 the target
    /// size and the target sizes of the levels look like:
    /// [- - 1.01MB 10.1MB 101MB]
    /// In the same way, while level 5 further grows, all levels' targets grow,
    /// like
    /// [- - 5MB 50MB 500MB]
    /// Until level 5 exceeds 1000MB and becomes 1001MB, we make level 2 the
    /// base level and make levels' target sizes like this:
    /// [- 1.001MB 10.01MB 100.1MB 1001MB]
    /// and go on...
    ///
    /// By doing it, we give max_bytes_for_level_multiplier a priority against
    /// max_bytes_for_level_base, for a more predictable LSM tree shape. It is
    /// useful to limit worse case space amplification.
    ///
    /// max_bytes_for_level_multiplier_additional is ignored with this flag on.
    ///
    /// Turning this feature on or off for an existing DB can cause unexpected
    /// LSM tree structure so it's not recommended.
    ///
    /// NOTE: this option is experimental
    ///
    /// Default: false
    pub level_compaction_dynamic_level_bytes: bool,

    /// Default: 10.
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_bytes_for_level_multiplier: f64,

    /// Different max-size multipliers for different levels.
    /// These are multiplied by max_bytes_for_level_multiplier to arrive
    /// at the max-size of each level.
    ///
    /// Default: 1
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_bytes_for_level_multiplier_additional: Vec<i32>,

    /// We try to limit number of bytes in one compaction to be lower than this
    /// threshold. But it's not guaranteed.
    /// Value 0 will be sanitized.
    ///
    /// Default: result.target_file_size_base * 25
    pub max_compaction_bytes: u64,

    /// All writes will be slowed down to at least delayed_write_rate if estimated
    /// bytes needed to be compaction exceed this threshold.
    ///
    /// Default: 64GB
    pub soft_pending_compaction_bytes_limit: u64,

    /// All writes are stopped if estimated bytes needed to be compaction exceed
    /// this threshold.
    ///
    /// Default: 256GB
    pub hard_pending_compaction_bytes_limit: u64,

    /// size of one block in arena memory allocation.
    /// If <= 0, a proper value is automatically calculated (usually 1/8 of
    /// writer_buffer_size, rounded up to a multiple of 4KB).
    ///
    /// There are two additional restriction of the The specified size:
    /// (1) size should be in the range of [4096, 2 << 30] and
    /// (2) be the multiple of the CPU word (which helps with the memory
    /// alignment).
    ///
    /// We'll automatically check and adjust the size number to make sure it
    /// conforms to the restrictions.
    ///
    /// Default: 0
    ///
    /// Dynamically changeable through SetOptions() API
    pub arena_block_size: usize,

    /// Disable automatic compactions. Manual compactions can still
    /// be issued on this column family
    ///
    /// Dynamically changeable through SetOptions() API
    pub disable_auto_compactions: bool,

    /// The compaction style. Default: kCompactionStyleLevel
    pub compaction_style: CompactionStyle,

    /// If level compaction_style = kCompactionStyleLevel, for each level,
    /// which files are prioritized to be picked to compact.
    /// Default: kByCompensatedSize
    pub compaction_pri: CompactionPri,

    /// If true, compaction will verify checksum on every read that happens
    /// as part of compaction
    ///
    /// Default: true
    ///
    /// Dynamically changeable through SetOptions() API
    pub verify_checksums_in_compaction: bool,

    /// The options needed to support Universal Style compactions
    pub compaction_options_universal: CompactionOptionsUniversal,

    /// The options for FIFO compaction style
    pub compaction_options_fifo: CompactionOptionsFIFO,

    /// An iteration->Next() sequentially skips over keys with the same
    /// user-key unless this option is set. This number specifies the number
    /// of keys (with the same userkey) that will be sequentially
    /// skipped before a reseek is issued.
    ///
    /// Default: 8
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_sequential_skip_in_iterations: u64,

    /// This is a factory that provides MemTableRep objects.
    /// Default: a factory that provides a skip-list-based implementation of
    /// MemTableRep.
    // memtable_factory:
    /// This is a factory that provides TableFactory objects.
    /// Default: a block-based table factory that provides a default
    /// implementation of TableBuilder and TableReader with default
    /// BlockBasedTableOptions.
    pub table_factory: Option<TableFactory>,

    /// Block-based table related options are moved to BlockBasedTableOptions.
    /// Related options that were originally here but now moved include:
    ///   no_block_cache
    ///   block_cache
    ///   block_cache_compressed
    ///   block_size
    ///   block_size_deviation
    ///   block_restart_interval
    ///   filter_policy
    ///   whole_key_filtering
    /// If you'd like to customize some of these options, you will need to
    /// use NewBlockBasedTableFactory() to construct a new table factory.

    /// This option allows user to collect their own interested statistics of
    /// the tables.
    /// Default: empty vector -- no user-defined statistics collection will be
    /// performed.
    pub table_properties_collector_factories: Vec<()>,

    /// Allows thread-safe inplace updates. If this is true, there is no way to
    /// achieve point-in-time consistency using snapshot or iterator (assuming
    /// concurrent updates). Hence iterator and multi-get will return results
    /// which are not consistent as of any point-in-time.
    /// If inplace_callback function is not set,
    ///   Put(key, new_value) will update inplace the existing_value iff
    ///   * key exists in current memtable
    ///   * new sizeof(new_value) <= sizeof(existing_value)
    ///   * existing_value for that key is a put i.e. kTypeValue
    /// If inplace_callback function is set, check doc for inplace_callback.
    /// Default: false.
    pub inplace_update_support: bool,

    /// Number of locks used for inplace update
    /// Default: 10000, if inplace_update_support = true, else 0.
    ///
    /// Dynamically changeable through SetOptions() API
    pub inplace_update_num_locks: usize,

    /// existing_value - pointer to previous value (from both memtable and sst).
    ///                  pub nullptr if key doesn't exist
    /// existing_value_size - pointer to size of existing_value).
    ///                       pub nullptr if key doesn't exist
    /// delta_value - Delta value to be merged with the existing_value.
    ///               pub Stored in transaction logs.
    /// merged_value - Set when delta is applied on the previous value.

    /// Applicable only when inplace_update_support is true,
    /// this callback function is called at the time of updating the memtable
    /// as part of a Put operation, lets say Put(key, delta_value). It allows the
    /// 'delta_value' specified as part of the Put operation to be merged with
    /// an 'existing_value' of the key in the database.

    /// If the merged value is smaller in size that the 'existing_value',
    /// then this function can update the 'existing_value' buffer inplace and
    /// the corresponding 'existing_value'_size pointer, if it wishes to.
    /// The callback should return UpdateStatus::UPDATED_INPLACE.
    /// In this case. (In this case, the snapshot-semantics of the rocksdb
    /// Iterator is not atomic anymore).

    /// If the merged value is larger in size than the 'existing_value' or the
    /// application does not wish to modify the 'existing_value' buffer inplace,
    /// then the merged value should be returned via *merge_value. It is set by
    /// merging the 'existing_value' and the Put 'delta_value'. The callback should
    /// return UpdateStatus::UPDATED in this case. This merged value will be added
    /// to the memtable.

    /// If merging fails or the application does not wish to take any action,
    /// then the callback should return UpdateStatus::UPDATE_FAILED.

    /// Please remember that the original call from the application is Put(key,
    /// delta_value). So the transaction log (if enabled) will still contain (key,
    /// delta_value). The 'merged_value' is not stored in the transaction log.
    /// Hence the inplace_callback function should be consistent across db reopens.

    /// Default: nullptr
    pub inplace_callback: Option<()>,
    //  UpdateStatus (*inplace_callback)(char* existing_value,
    // uint32_t* existing_value_size,
    // Slice delta_value,
    // std::string* merged_value) = nullptr;
    /// if prefix_extractor is set and memtable_prefix_bloom_size_ratio is not 0,
    /// create prefix bloom for memtable with the size of
    /// write_buffer_size * memtable_prefix_bloom_size_ratio.
    /// If it is larger than 0.25, it is santinized to 0.25.
    ///
    /// Default: 0 (disable)
    ///
    /// Dynamically changeable through SetOptions() API
    pub memtable_prefix_bloom_size_ratio: f64,

    /// Page size for huge page for the arena used by the memtable. If <=0, it
    /// won't allocate from huge page but from malloc.
    /// Users are responsible to reserve huge pages for it to be allocated. For
    /// example:
    ///      pub sysctl -w vm.nr_hugepages=20
    /// See linux doc Documentation/vm/hugetlbpage.txt
    /// If there isn't enough free huge page available, it will fall back to
    /// malloc.
    ///
    /// Dynamically changeable through SetOptions() API
    pub memtable_huge_page_size: usize,

    /// If non-nullptr, memtable will use the specified function to extract
    /// prefixes for keys, and for each prefix maintain a hint of insert location
    /// to reduce CPU usage for inserting keys with the prefix. Keys out of
    /// domain of the prefix extractor will be insert without using hints.
    ///
    /// Currently only the default skiplist based memtable implements the feature.
    /// All other memtable implementation will ignore the option. It incurs ~250
    /// additional bytes of memory overhead to store a hint for each prefix.
    /// Also concurrent writes (when allow_concurrent_memtable_write is true) will
    /// ignore the option.
    ///
    /// The option is best suited for workloads where keys will likely to insert
    /// to a location close the the last inserted key with the same prefix.
    /// One example could be inserting keys of the form (prefix + timestamp),
    /// and keys of the same prefix always comes in with time order. Another
    /// example would be updating the same key over and over again, in which case
    /// the prefix can be the key itself.
    ///
    /// Default: nullptr (disable)
    pub memtable_insert_with_hint_prefix_extractor: Option<SliceTransform>,

    /// Control locality of bloom filter probes to improve cache miss rate.
    /// This option only applies to memtable prefix bloom and plaintable
    /// prefix bloom. It essentially limits every bloom checking to one cache line.
    /// This optimization is turned off when set to 0, and positive number to turn
    /// it on.
    /// Default: 0
    pub bloom_locality: u32,

    /// Maximum number of successive merge operations on a key in the memtable.
    ///
    /// When a merge operation is added to the memtable and the maximum number of
    /// successive merges is reached, the value of the key will be calculated and
    /// inserted into the memtable instead of the merge operation. This will
    /// ensure that there are never more than max_successive_merges merge
    /// operations in the memtable.
    ///
    /// Default: 0 (disabled)
    ///
    /// Dynamically changeable through SetOptions() API
    pub max_successive_merges: usize,

    /// The number of partial merge operands to accumulate before partial
    /// merge will be performed. Partial merge will not be called
    /// if the list of values to merge is less than min_partial_merge_operands.
    ///
    /// If min_partial_merge_operands < 2, then it will be treated as 2.
    ///
    /// Default: 2
    pub min_partial_merge_operands: u32,

    /// This flag specifies that the implementation should optimize the filters
    /// mainly for cases where keys are found rather than also optimize for keys
    /// missed. This would be used in cases where the application knows that
    /// there are very few misses or the performance in the case of misses is not
    /// important.
    ///
    /// For now, this flag allows us to not store filters for the last level i.e
    /// the largest level which contains data of the LSM store. For keys which
    /// are hits, the filters in this level are not useful because we will search
    /// for the data anyway. NOTE: the filters in other levels are still useful
    /// even for key hit because they tell us whether to look in that level or go
    /// to the higher level.
    ///
    /// Default: false
    pub optimize_filters_for_hits: bool,

    /// After writing every SST file, reopen it and read all the keys.
    /// Default: false
    pub paranoid_file_checks: bool,

    /// In debug mode, RocksDB run consistency checks on the LSM everytime the LSM
    /// change (Flush, Compaction, AddFile). These checks are disabled in release
    /// mode, use this option to enable them in release mode as well.
    /// Default: false
    pub force_consistency_checks: bool,

    /// Measure IO stats in compactions and flushes, if true.
    /// Default: false
    pub report_bg_io_stats: bool,
}


impl Default for ColumnFamilyOptions {
    fn default() -> Self {
        let default_num_levels = 7;

        ColumnFamilyOptions {
            comparator: None, // BytewiseComparator,
            merge_operator: None,
            compaction_filter: None,
            compaction_filter_factory: None,
            write_buffer_size: 64 << 20,
            max_write_buffer_number: 2,
            min_write_buffer_number_to_merge: 1,
            max_write_buffer_number_to_maintain: 0,
            // Default: kSnappyCompression, if it's supported. If snappy is not linked
            // with the library, the default is kNoCompression.
            compression: CompressionType::NoCompression,
            compression_per_level: vec![],
            bottommost_compression: CompressionType::DisableCompressionOption,
            compression_opts: CompressionOptions::default(),
            prefix_extractor: None,
            num_levels: default_num_levels,
            level0_file_num_compaction_trigger: 4,
            level0_slowdown_writes_trigger: 20,
            level0_stop_writes_trigger: 36,
            target_file_size_base: 64 * 1048576,
            target_file_size_multiplier: 1,
            max_bytes_for_level_base: 256 * 1048576,
            level_compaction_dynamic_level_bytes: false,
            max_bytes_for_level_multiplier: 10.0,
            max_bytes_for_level_multiplier_additional: vec![1; default_num_levels as usize],
            max_compaction_bytes: 0,
            soft_pending_compaction_bytes_limit: 64 * 1073741824,
            hard_pending_compaction_bytes_limit: 256 * 1073741824,
            arena_block_size: 0,
            disable_auto_compactions: false,
            compaction_style: CompactionStyle::CompactionStyleLevel,
            compaction_pri: CompactionPri::ByCompensatedSize,
            verify_checksums_in_compaction: true,
            compaction_options_universal: CompactionOptionsUniversal::default(),
            compaction_options_fifo: Default::default(),
            max_sequential_skip_in_iterations: 8,
            // memtable_factory: None,
            //      std::shared_ptr<SkipListFactory>(new SkipListFactory),
            // typedef std::vector<std::shared_ptr<TablePropertiesCollectorFactory>>
            table_factory: None,
            table_properties_collector_factories: Default::default(),
            inplace_update_support: false,
            inplace_update_num_locks: 10000,
            inplace_callback: None,
            memtable_prefix_bloom_size_ratio: 0.0,
            memtable_huge_page_size: 0,
            memtable_insert_with_hint_prefix_extractor: None,
            bloom_locality: 0,
            max_successive_merges: 0,
            min_partial_merge_operands: 2,
            optimize_filters_for_hits: false,
            paranoid_file_checks: false,
            force_consistency_checks: false,
            report_bg_io_stats: false,
        }
    }
}

impl ColumnFamilyOptions {
    /// The function recovers options to a previous version. Only 4.6 or later
    /// versions are supported.
    pub fn old_defaults(rocksdb_major_version: i32, irocksdb_minor_version: i32) -> Self {
        unimplemented!()
    }

    /// Some functions that make it easier to optimize RocksDB
    /// Use this if your DB is very small (like under 1GB) and you don't want to
    /// spend lots of memory for memtables.
    pub fn optimize_for_smalldb(&mut self) -> &mut Self {
        unimplemented!();
        self
    }

    /// Use this if you don't need to keep the data sorted, i.e. you'll never use
    /// an iterator, only Put() and Get() API calls
    ///
    /// Not supported in ROCKSDB_LITE
    pub fn optimize_for_pointlookup(&mut self, block_cache_size_mb: u64) -> &mut Self {
        unimplemented!();
        self
    }

    /// Default values for some parameters in ColumnFamilyOptions are not
    /// optimized for heavy workloads and big datasets, which means you might
    /// observe write stalls under some conditions. As a starting point for tuning
    /// RocksDB options, use the following two functions:
    /// * OptimizeLevelStyleCompaction -- optimizes level style compaction
    /// * OptimizeUniversalStyleCompaction -- optimizes universal style compaction
    /// Universal style compaction is focused on reducing Write Amplification
    /// Factor for big data sets, but increases Space Amplification. You can learn
    /// more about the different styles here:
    /// https://github.com/facebook/rocksdb/wiki/Rocksdb-Architecture-Guide
    /// Make sure to also call IncreaseParallelism(), which will provide the
    /// biggest performance gains.
    /// Note: we might use more memory than memtable_memory_budget during high
    /// write rate period
    ///
    /// OptimizeUniversalStyleCompaction is not supported in ROCKSDB_LITE
    pub fn optimize_level_style_compaction(&mut self, memtable_memory_budget: u64) -> &mut Self {
        // 512 * 1024 * 1024);
        unimplemented!();
        self
    }

    pub fn optimize_universal_style_compaction(&mut self,
                                               memtable_memory_budget: u64)
                                               -> &mut Self {
        // 512 * 1024 * 1024)
        unimplemented!();
        self
    }

    // Create ColumnFamilyOptions with default values for all fields
    // ColumnFamilyOptions();
    // Create ColumnFamilyOptions from Options
    // explicit ColumnFamilyOptions(const Options& options);
    //
    pub fn dump(&self, log: &mut Logger) {
        unimplemented!()
    }
}


/// Specify the file access pattern once a compaction is started.
/// It will be applied to all input files of a compaction.
/// Default: NORMAL
#[repr(C)]
pub enum AccessHint {
    None,
    Normal,
    Sequential,
    WillNeed,
}

pub struct DBOptions {
    /// If true, the database will be created if it is missing.
    /// Default: false
    pub create_if_missing: bool,

    /// If true, missing column families will be automatically created.
    /// Default: false
    pub create_missing_column_families: bool,

    /// If true, an error is raised if the database already exists.
    /// Default: false
    pub error_if_exists: bool,

    /// If true, RocksDB will aggressively check consistency of the data.
    /// Also, if any of the  writes to the database fails (Put, Delete, Merge,
    /// Write), the database will switch to read-only mode and fail all other
    /// Write operations.
    /// In most cases you want this to be set to true.
    /// Default: true
    pub paranoid_checks: bool,

    /// Use the specified object to interact with the environment,
    /// e.g. to read/write files, schedule background work, etc.
    /// Default: Env::Default()
    // env: Env,
    /// Use to control write rate of flush and compaction. Flush has higher
    /// priority than compaction. Rate limiting is disabled if nullptr.
    /// If rate limiter is enabled, bytes_per_sync is set to 1MB by default.
    /// Default: nullptr
    pub rate_limiter: Option<RateLimiter>,

    /// Use to track SST files and control their file deletion rate.
    ///
    /// Features:
    ///  - Throttle the deletion rate of the SST files.
    ///  - Keep track the total size of all SST files.
    ///  - Set a maximum allowed space limit for SST files that when reached
    ///    the DB wont do any further flushes or compactions and will set the
    ///    background error.
    ///  - Can be shared between multiple dbs.
    /// Limitations:
    ///  - Only track and throttle deletes of SST files in
    ///    first db_path (db_name if db_paths is empty).
    ///
    /// Default: nullptr
    pub sst_file_manager: Option<SstFileManager>,

    /// Any internal progress/error information generated by the db will
    /// be written to info_log if it is non-nullptr, or to a file stored
    /// in the same directory as the DB contents if info_log is nullptr.
    /// Default: nullptr
    pub info_log: Option<Logger>,

    pub info_log_level: InfoLogLevel,

    /// Number of open files that can be used by the DB.  You may need to
    /// increase this if your database has a large working set. Value -1 means
    /// files opened are always kept open. You can estimate number of files based
    /// on target_file_size_base and target_file_size_multiplier for level-based
    /// compaction. For universal-style compaction, you can usually set it to -1.
    /// Default: -1
    pub max_open_files: i32,

    /// If max_open_files is -1, DB will open all files on DB::Open(). You can
    /// use this option to increase the number of threads used to open the files.
    /// Default: 16
    pub max_file_opening_threads: i32,

    /// Once write-ahead logs exceed this size, we will start forcing the flush of
    /// column families whose memtables are backed by the oldest live WAL file
    /// (i.e. the ones that are causing all the space amplification). If set to 0
    /// (default), we will dynamically choose the WAL size limit to be
    /// [sum of all write_buffer_size * max_write_buffer_number] * 4
    /// Default: 0
    pub max_total_wal_size: u64,

    /// If non-null, then we should collect metrics about database operations
    pub statistics: Option<Statistics>,

    /// If true, then every store to stable storage will issue a fsync.
    /// If false, then every store to stable storage will issue a fdatasync.
    /// This parameter should be set to true while storing data to
    /// filesystem like ext3 that can lose files after a reboot.
    /// Default: false
    /// Note: on many platforms fdatasync is defined as fsync, so this parameter
    /// would make no difference. Refer to fdatasync definition in this code base.
    pub use_fsync: bool,

    /// A list of paths where SST files can be put into, with its target size.
    /// Newer data is placed into paths specified earlier in the vector while
    /// older data gradually moves to paths specified later in the vector.
    ///
    /// For example, you have a flash device with 10GB allocated for the DB,
    /// as well as a hard drive of 2TB, you should config it to be:
    ///   [{"/flash_path", 10GB}, {"/hard_drive", 2TB}]
    ///
    /// The system will try to guarantee data under each path is close to but
    /// not larger than the target size. But current and future file sizes used
    /// by determining where to place a file are based on best-effort estimation,
    /// which means there is a chance that the actual size under the directory
    /// is slightly more than target size under some workloads. User should give
    /// some buffer room for those cases.
    ///
    /// If none of the paths has sufficient room to place a file, the file will
    /// be placed to the last path anyway, despite to the target size.
    ///
    /// Placing newer data to earlier paths is also best-efforts. User should
    /// expect user files to be placed in higher levels in some extreme cases.
    ///
    /// If left empty, only one path will be used, which is db_name passed when
    /// opening the DB.
    /// Default: empty
    pub db_paths: Vec<DbPath>,

    /// This specifies the info LOG dir.
    /// If it is empty, the log files will be in the same dir as data.
    /// If it is non empty, the log files will be in the specified dir,
    /// and the db data dir's absolute path will be used as the log file
    /// name's prefix.
    pub db_log_dir: String,

    /// This specifies the absolute dir path for write-ahead logs (WAL).
    /// If it is empty, the log files will be in the same dir as data,
    ///   dbname is used as the data dir by default
    /// If it is non empty, the log files will be in kept the specified dir.
    /// When destroying the db,
    ///   all log files in wal_dir and the dir itself is deleted
    pub wal_dir: String,

    /// The periodicity when obsolete files get deleted. The default
    /// value is 6 hours. The files that get out of scope by compaction
    /// process will still get automatically delete on every compaction,
    /// regardless of this setting
    pub delete_obsolete_files_period_micros: u64,

    /// Suggested number of concurrent background compaction jobs, submitted to
    /// the default LOW priority thread pool.
    ///
    /// Default: 1
    pub base_background_compactions: i32,

    /// Maximum number of concurrent background compaction jobs, submitted to
    /// the default LOW priority thread pool.
    /// We first try to schedule compactions based on
    /// `base_background_compactions`. If the compaction cannot catch up , we
    /// will increase number of compaction threads up to
    /// `max_background_compactions`.
    ///
    /// If you're increasing this, also consider increasing number of threads in
    /// LOW priority thread pool. For more information, see
    /// Env::SetBackgroundThreads
    /// Default: 1
    pub max_background_compactions: i32,

    /// This value represents the maximum number of threads that will
    /// concurrently perform a compaction job by breaking it into multiple,
    /// smaller ones that are run simultaneously.
    /// Default: 1 (i.e. no subcompactions)
    pub max_subcompactions: u32,

    /// Maximum number of concurrent background memtable flush jobs, submitted to
    /// the HIGH priority thread pool.
    ///
    /// By default, all background jobs (major compaction and memtable flush) go
    /// to the LOW priority pool. If this option is set to a positive number,
    /// memtable flush jobs will be submitted to the HIGH priority pool.
    /// It is important when the same Env is shared by multiple db instances.
    /// Without a separate pool, long running major compaction jobs could
    /// potentially block memtable flush jobs of other db instances, leading to
    /// unnecessary Put stalls.
    ///
    /// If you're increasing this, also consider increasing number of threads in
    /// HIGH priority thread pool. For more information, see
    /// Env::SetBackgroundThreads
    /// Default: 1
    pub max_background_flushes: i32,

    /// Specify the maximal size of the info log file. If the log file
    /// is larger than `max_log_file_size`, a new info log file will
    /// be created.
    /// If max_log_file_size == 0, all logs will be written to one
    /// log file.
    pub max_log_file_size: usize,

    /// Time for the info log file to roll (in seconds).
    /// If specified with non-zero value, log file will be rolled
    /// if it has been active longer than `log_file_time_to_roll`.
    /// Default: 0 (disabled)
    /// Not supported in ROCKSDB_LITE mode!
    pub log_file_time_to_roll: usize,

    /// Maximal info log files to be kept.
    /// Default: 1000
    pub keep_log_file_num: usize,

    /// Recycle log files.
    /// If non-zero, we will reuse previously written log files for new
    /// logs, overwriting the old data.  The value indicates how many
    /// such files we will keep around at any point in time for later
    /// use.  This is more efficient because the blocks are already
    /// allocated and fdatasync does not need to update the inode after
    /// each write.
    /// Default: 0
    pub recycle_log_file_num: usize,

    /// manifest file is rolled over on reaching this limit.
    /// The older manifest file be deleted.
    /// The default value is MAX_INT so that roll-over does not take place.
    pub max_manifest_file_size: u64,

    /// Number of shards used for table cache.
    pub table_cache_numshardbits: i32,

    /// The following two fields affect how archived logs will be deleted.
    /// 1. If both set to 0, logs will be deleted asap and will not get into
    ///    the archive.
    /// 2. If WAL_ttl_seconds is 0 and WAL_size_limit_MB is not 0,
    ///    WAL files will be checked every 10 min and if total size is greater
    ///    then WAL_size_limit_MB, they will be deleted starting with the
    ///    earliest until size_limit is met. All empty files will be deleted.
    /// 3. If WAL_ttl_seconds is not 0 and WAL_size_limit_MB is 0, then
    ///    WAL files will be checked every WAL_ttl_secondsi / 2 and those that
    ///    are older than WAL_ttl_seconds will be deleted.
    /// 4. If both are not 0, WAL files will be checked every 10 min and both
    ///    checks will be performed with ttl being first.
    pub WAL_ttl_seconds: u64,
    pub WAL_size_limit_MB: u64,

    /// Number of bytes to preallocate (via fallocate) the manifest
    /// files.  Default is 4mb, which is reasonable to reduce random IO
    /// as well as prevent overallocation for mounts that preallocate
    /// large amounts of data (such as xfs's allocsize option).
    pub manifest_preallocation_size: usize,

    /// Allow the OS to mmap file for reading sst tables. Default: false
    pub allow_mmap_reads: bool,

    /// Allow the OS to mmap file for writing.
    /// DB::SyncWAL() only works if this is set to false.
    /// Default: false
    pub allow_mmap_writes: bool,

    /// Enable direct I/O mode for read/write
    /// they may or may not improve performance depending on the use case
    ///
    /// Files will be opened in "direct I/O" mode
    /// which means that data r/w from the disk will not be cached or
    /// bufferized. The hardware buffer of the devices may however still
    /// be used. Memory mapped files are not impacted by these parameters.

    /// Use O_DIRECT for user reads
    /// Default: false
    /// Not supported in ROCKSDB_LITE mode!
    pub use_direct_reads: bool,

    /// Use O_DIRECT for both reads and writes in background flush and compactions
    /// When true, we also force new_table_reader_for_compaction_inputs to true.
    /// Default: false
    /// Not supported in ROCKSDB_LITE mode!
    pub use_direct_io_for_flush_and_compaction: bool,

    /// If false, fallocate() calls are bypassed
    pub allow_fallocate: bool,

    /// Disable child process inherit open files. Default: true
    pub is_fd_close_on_exec: bool,

    /// NOT SUPPORTED ANYMORE -- this options is no longer used
    pub skip_log_error_on_recovery: bool,

    /// if not zero, dump rocksdb.stats to LOG every stats_dump_period_sec
    /// Default: 600 (10 min)
    pub stats_dump_period_sec: i32,

    /// If set true, will hint the underlying file system that the file
    /// access pattern is random, when a sst file is opened.
    /// Default: true
    pub advise_random_on_open: bool,

    /// Amount of data to build up in memtables across all column
    /// families before writing to disk.
    ///
    /// This is distinct from write_buffer_size, which enforces a limit
    /// for a single memtable.
    ///
    /// This feature is disabled by default. Specify a non-zero value
    /// to enable it.
    ///
    /// Default: 0 (disabled)
    pub db_write_buffer_size: usize,

    /// The memory usage of memtable will report to this object. The same object
    /// can be passed into multiple DBs and it will track the sum of size of all
    /// the DBs. If the total size of all live memtables of all the DBs exceeds
    /// a limit, a flush will be triggered in the next DB to which the next write
    /// is issued.
    ///
    /// If the object is only passed to on DB, the behavior is the same as
    /// db_write_buffer_size. When write_buffer_manager is set, the value set will
    /// override db_write_buffer_size.
    ///
    /// This feature is disabled by default. Specify a non-zero value
    /// to enable it.
    ///
    /// Default: null
    pub write_buffer_manager: Option<WriteBufferManager>,

    /// Specify the file access pattern once a compaction is started.
    /// It will be applied to all input files of a compaction.
    /// Default: NORMAL
    pub access_hint_on_compaction_start: AccessHint,

    /// If true, always create a new file descriptor and new table reader
    /// for compaction inputs. Turn this parameter on may introduce extra
    /// memory usage in the table reader, if it allocates extra memory
    /// for indexes. This will allow file descriptor prefetch options
    /// to be set for compaction input files and not to impact file
    /// descriptors for the same file used by user queries.
    /// Suggest to enable BlockBasedTableOptions.cache_index_and_filter_blocks
    /// for this mode if using block-based table.
    ///
    /// Default: false
    pub new_table_reader_for_compaction_inputs: bool,

    /// If non-zero, we perform bigger reads when doing compaction. If you're
    /// running RocksDB on spinning disks, you should set this to at least 2MB.
    /// That way RocksDB's compaction is doing sequential instead of random reads.
    ///
    /// When non-zero, we also force new_table_reader_for_compaction_inputs to
    /// true.
    ///
    /// Default: 0
    pub compaction_readahead_size: usize,

    /// This is a maximum buffer size that is used by WinMmapReadableFile in
    /// unbuffered disk I/O mode. We need to maintain an aligned buffer for
    /// reads. We allow the buffer to grow until the specified value and then
    /// for bigger requests allocate one shot buffers. In unbuffered mode we
    /// always bypass read-ahead buffer at ReadaheadRandomAccessFile
    /// When read-ahead is required we then make use of compaction_readahead_size
    /// value and always try to read ahead. With read-ahead we always
    /// pre-allocate buffer to the size instead of growing it up to a limit.
    ///
    /// This option is currently honored only on Windows
    ///
    /// Default: 1 Mb
    ///
    /// Special value: 0 - means do not maintain per instance buffer. Allocate
    ///                per request buffer and avoid locking.
    pub random_access_max_buffer_size: usize,

    /// This is the maximum buffer size that is used by WritableFileWriter.
    /// On Windows, we need to maintain an aligned buffer for writes.
    /// We allow the buffer to grow until it's size hits the limit in buffered
    /// IO and fix the buffer size when using direct IO to ensure alignment of
    /// write requests if the logical sector size is unusual
    ///
    /// Default: 1024 * 1024 (1 MB)
    pub writable_file_max_buffer_size: usize,

    /// Use adaptive mutex, which spins in the user space before resorting
    /// to kernel. This could reduce context switch when the mutex is not
    /// heavily contended. However, if the mutex is hot, we could end up
    /// wasting spin time.
    /// Default: false
    pub use_adaptive_mutex: bool,

    /// Allows OS to incrementally sync files to disk while they are being
    /// written, asynchronously, in the background. This operation can be used
    /// to smooth out write I/Os over time. Users shouldn't rely on it for
    /// persistency guarantee.
    /// Issue one request for every bytes_per_sync written. 0 turns it off.
    /// Default: 0
    ///
    /// You may consider using rate_limiter to regulate write rate to device.
    /// When rate limiter is enabled, it automatically enables bytes_per_sync
    /// to 1MB.
    ///
    /// This option applies to table files
    pub bytes_per_sync: u64,

    /// Same as bytes_per_sync, but applies to WAL files
    /// Default: 0, turned off
    pub wal_bytes_per_sync: u64,

    /// A vector of EventListeners which call-back functions will be called
    /// when specific RocksDB event happens.
    pub listeners: Vec<EventListener>,

    /// If true, then the status of the threads involved in this DB will
    /// be tracked and available via GetThreadList() API.
    ///
    /// Default: false
    pub enable_thread_tracking: bool,

    /// The limited write rate to DB if soft_pending_compaction_bytes_limit or
    /// level0_slowdown_writes_trigger is triggered, or we are writing to the
    /// last mem table allowed and we allow more than 3 mem tables. It is
    /// calculated using size of user write requests before compression.
    /// RocksDB may decide to slow down more if the compaction still
    /// gets behind further.
    /// Unit: byte per second.
    ///
    /// Default: 16MB/s
    pub delayed_write_rate: u64,

    /// If true, allow multi-writers to update mem tables in parallel.
    /// Only some memtable_factory-s support concurrent writes; currently it
    /// is implemented only for SkipListFactory.  Concurrent memtable writes
    /// are not compatible with inplace_update_support or filter_deletes.
    /// It is strongly recommended to set enable_write_thread_adaptive_yield
    /// if you are going to use this feature.
    ///
    /// Default: true
    pub allow_concurrent_memtable_write: bool,

    /// If true, threads synchronizing with the write batch group leader will
    /// wait for up to write_thread_max_yield_usec before blocking on a mutex.
    /// This can substantially improve throughput for concurrent workloads,
    /// regardless of whether allow_concurrent_memtable_write is enabled.
    ///
    /// Default: true
    pub enable_write_thread_adaptive_yield: bool,

    /// The maximum number of microseconds that a write operation will use
    /// a yielding spin loop to coordinate with other write threads before
    /// blocking on a mutex.  (Assuming write_thread_slow_yield_usec is
    /// set properly) increasing this value is likely to increase RocksDB
    /// throughput at the expense of increased CPU usage.
    ///
    /// Default: 100
    pub write_thread_max_yield_usec: u64,

    /// The latency in microseconds after which a std::this_thread::yield
    /// call (sched_yield on Linux) is considered to be a signal that
    /// other processes or threads would like to use the current core.
    /// Increasing this makes writer threads more likely to take CPU
    /// by spinning, which will show up as an increase in the number of
    /// involuntary context switches.
    ///
    /// Default: 3
    pub write_thread_slow_yield_usec: u64,

    /// If true, then DB::Open() will not update the statistics used to optimize
    /// compaction decision by loading table properties from many files.
    /// Turning off this feature will improve DBOpen time especially in
    /// disk environment.
    ///
    /// Default: false
    pub skip_stats_update_on_db_open: bool,

    /// Recovery mode to control the consistency while replaying WAL
    /// Default: kPointInTimeRecovery
    pub wal_recovery_mode: WALRecoveryMode,

    /// if set to false then recovery will fail when a prepared
    /// transaction is encountered in the WAL
    pub allow_2pc: bool,

    /// A global cache for table-level rows.
    /// Default: nullptr (disabled)
    /// Not supported in ROCKSDB_LITE mode!
    pub row_cache: Option<Cache>,

    // #ifndef ROCKSDB_LITE
    // /// A filter object supplied to be invoked while processing write-ahead-logs
    // /// (WALs) during recovery. The filter provides a way to inspect log
    // /// records, ignoring a particular record or skipping replay.
    // /// The filter is invoked at startup and is invoked from a single-thread
    // /// currently.
    // WalFilter* wal_filter ,
    // #endif  /// ROCKSDB_LITE
    /// If true, then DB::Open / CreateColumnFamily / DropColumnFamily
    /// / SetOptions will fail if options file is not detected or properly
    /// persisted.
    ///
    /// DEFAULT: false
    pub fail_if_options_file_error: bool,

    /// If true, then print malloc stats together with rocksdb.stats
    /// when printing to LOG.
    /// DEFAULT: false
    pub dump_malloc_stats: bool,

    /// By default RocksDB replay WAL logs and flush them on DB open, which may
    /// create very small SST files. If this option is enabled, RocksDB will try
    /// to avoid (but not guarantee not to) flush during recovery. Also, existing
    /// WAL logs will be kept, so that if crash happened before flush, we still
    /// have logs to recover from.
    ///
    /// DEFAULT: false
    pub avoid_flush_during_recovery: bool,

    /// By default RocksDB will flush all memtables on DB close if there are
    /// unpersisted data (i.e. with WAL disabled) The flush can be skip to speedup
    /// DB close. Unpersisted data WILL BE LOST.
    ///
    /// DEFAULT: false
    ///
    /// Dynamically changeable through SetDBOptions() API.
    pub avoid_flush_during_shutdown: bool,
}

impl Default for DBOptions {
    fn default() -> Self {
        DBOptions {
            create_if_missing: false,
            create_missing_column_families: false,
            error_if_exists: false,
            paranoid_checks: true,
            // env: Env::Default(),
            rate_limiter: None,
            sst_file_manager: None,
            info_log: None,
            info_log_level: InfoLogLevel::Info,
            max_open_files: -1,
            max_file_opening_threads: 16,
            max_total_wal_size: 0,
            statistics: None,
            use_fsync: false,
            db_paths: Vec::new(),
            db_log_dir: "".to_string(),
            wal_dir: "".to_string(),
            delete_obsolete_files_period_micros: 6 * 60 * 60 * 1000000,
            base_background_compactions: 1,
            max_background_compactions: 1,
            max_subcompactions: 1,
            max_background_flushes: 1,
            max_log_file_size: 0,
            log_file_time_to_roll: 0,
            keep_log_file_num: 1000,
            recycle_log_file_num: 0,
            max_manifest_file_size: u64::MAX,
            table_cache_numshardbits: 6,
            WAL_ttl_seconds: 0,
            WAL_size_limit_MB: 0,
            manifest_preallocation_size: 4 * 1024 * 1024,
            allow_mmap_reads: false,
            allow_mmap_writes: false,
            use_direct_reads: false,
            use_direct_io_for_flush_and_compaction: false,
            allow_fallocate: true,
            is_fd_close_on_exec: true,
            skip_log_error_on_recovery: false,
            stats_dump_period_sec: 600,
            advise_random_on_open: true,
            db_write_buffer_size: 0,
            write_buffer_manager: None,
            access_hint_on_compaction_start: AccessHint::Normal,
            new_table_reader_for_compaction_inputs: false,
            compaction_readahead_size: 0,
            random_access_max_buffer_size: 1024 * 1024,
            writable_file_max_buffer_size: 1024 * 1024,
            use_adaptive_mutex: false,
            bytes_per_sync: 0,
            wal_bytes_per_sync: 0,
            listeners: Vec::new(),
            enable_thread_tracking: false,
            delayed_write_rate: 16 * 1024 * 1024,
            allow_concurrent_memtable_write: true,
            enable_write_thread_adaptive_yield: true,
            write_thread_max_yield_usec: 100,
            write_thread_slow_yield_usec: 3,
            skip_stats_update_on_db_open: false,
            wal_recovery_mode: WALRecoveryMode::PointInTimeRecovery,
            allow_2pc: false,
            row_cache: None,
            // wal_filter: None,
            fail_if_options_file_error: false,
            dump_malloc_stats: false,
            avoid_flush_during_recovery: false,
            avoid_flush_during_shutdown: false,
        }
    }
}

/// Options to control the behavior of a database (passed to DB::Open)
pub struct Options {
    db: DBOptions,
    cf: ColumnFamilyOptions,
}

impl Options {
    // Some functions that make it easier to optimize RocksDB

    /// Set appropriate parameters for bulk loading.
    /// The reason that this is a function that returns "this" instead of a
    /// constructor is to enable chaining of multiple similar calls in the future.
    ///

    /// All data will be in level 0 without any automatic compaction.
    /// It's recommended to manually call CompactRange(NULL, NULL) before reading
    /// from the database, because otherwise the read can be very slow.
    pub fn prepare_for_bulk_load(&mut self) -> &mut Self {
        unimplemented!()
    }

    /// Use this if your DB is very small (like under 1GB) and you don't want to
    /// spend lots of memory for memtables.
    pub fn optimize_for_small_db(&mut self) -> &mut Self {
        unimplemented!()
    }
}

/// An application can issue a read request (via Get/Iterators) and specify
/// if that read should process data that ALREADY resides on a specified cache
/// level. For example, if an application specifies kBlockCacheTier then the
/// Get call will process data that is already processed in the memtable or
/// the block cache. It will not page in data from the OS cache or data that
/// resides in storage.
#[repr(C)]
pub enum ReadTier {
    /// data in memtable, block cache, OS cache or storage
    ReadAllTier = 0x0,
    /// data in memtable or block cache
    BlockCacheTier = 0x1,
    /// persisted data.  When WAL is disabled, this option
    /// will skip data in memtable.
    /// Note that this ReadTier currently only supports
    /// Get and MultiGet and does not support iterators.
    PersistedTier = 0x2,
}

/// Options that control read operations
pub struct ReadOptions {
    /// If true, all data read from underlying storage will be
    /// verified against corresponding checksums.
    /// Default: true
    pub verify_checksums: bool,

    /// Should the "data block"/"index block"/"filter block" read for this
    /// iteration be cached in memory?
    /// Callers may wish to set this field to false for bulk scans.
    /// Default: true
    pub fill_cache: bool,

    /// If this option is set and memtable implementation allows, Seek
    /// might only return keys with the same prefix as the seek-key
    ///
    /// ! DEPRECATED: prefix_seek is on by default when prefix_extractor
    /// is configured
    /// bool prefix_seek;

    /// If "snapshot" is non-nullptr, read as of the supplied snapshot
    /// (which must belong to the DB that is being read and which must
    /// not have been released).  If "snapshot" is nullptr, use an implicit
    /// snapshot of the state at the beginning of this read operation.
    /// Default: nullptr
    pub snapshot: Option<Snapshot>,

    /// "iterate_upper_bound" defines the extent upto which the forward iterator
    /// can returns entries. Once the bound is reached, Valid() will be false.
    /// "iterate_upper_bound" is exclusive ie the bound value is
    /// not a valid entry.  If iterator_extractor is not null, the Seek target
    /// and iterator_upper_bound need to have the same prefix.
    /// This is because ordering is not guaranteed outside of prefix domain.
    /// There is no lower bound on the iterator. If needed, that can be easily
    /// implemented
    ///
    /// Default: nullptr
    pub iterate_upper_bound: Option<Vec<u8>>,

    /// Specify if this read request should process data that ALREADY
    /// resides on a particular cache. If the required data is not
    /// found at the specified cache, then Status::Incomplete is returned.
    /// Default: kReadAllTier
    pub read_tier: ReadTier,

    /// Specify to create a tailing iterator -- a special iterator that has a
    /// view of the complete database (i.e. it can also be used to read newly
    /// added data) and is optimized for sequential reads. It will return records
    /// that were inserted into the database after the creation of the iterator.
    /// Default: false
    /// Not supported in ROCKSDB_LITE mode!
    pub tailing: bool,

    /// Specify to create a managed iterator -- a special iterator that
    /// uses less resources by having the ability to free its underlying
    /// resources on request.
    /// Default: false
    /// Not supported in ROCKSDB_LITE mode!
    pub managed: bool,

    /// Enable a total order seek regardless of index format (e.g. hash index)
    /// used in the table. Some table format (e.g. plain table) may not support
    /// this option.
    /// If true when calling Get(), we also skip prefix bloom when reading from
    /// block based table. It provides a way to read existing data after
    /// changing implementation of prefix extractor.
    pub total_order_seek: bool,

    /// Enforce that the iterator only iterates over the same prefix as the seek.
    /// This option is effective only for prefix seeks, i.e. prefix_extractor is
    /// non-null for the column family and total_order_seek is false.  Unlike
    /// iterate_upper_bound, prefix_same_as_start only works within a prefix
    /// but in both directions.
    /// Default: false
    pub prefix_same_as_start: bool,

    /// Keep the blocks loaded by the iterator pinned in memory as long as the
    /// iterator is not deleted, If used when reading from tables created with
    /// BlockBasedTableOptions::use_delta_encoding = false,
    /// Iterator's property "rocksdb.iterator.is-key-pinned" is guaranteed to
    /// return 1.
    /// Default: false
    pub pin_data: bool,

    /// If true, when PurgeObsoleteFile is called in CleanupIteratorState, we
    /// schedule a background job in the flush job queue and delete obsolete files
    /// in background.
    /// Default: false
    pub background_purge_on_iterator_cleanup: bool,

    /// If non-zero, NewIterator will create a new table reader which
    /// performs reads of the given size. Using a large size (> 2MB) can
    /// improve the performance of forward iteration on spinning disks.
    /// Default: 0
    pub readahead_size: usize,

    /// If true, keys deleted using the DeleteRange() API will be visible to
    /// readers until they are naturally deleted during compaction. This improves
    /// read performance in DBs with many range deletions.
    /// Default: false
    pub ignore_range_deletions: bool,
}

impl ReadOptions {
    pub fn new(cksum: bool, cache: bool) -> ReadOptions {
        unimplemented!()
    }
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            verify_checksums: true,
            fill_cache: true,
            snapshot: None,
            iterate_upper_bound: None,
            read_tier: ReadTier::ReadAllTier,
            tailing: false,
            managed: false,
            total_order_seek: false,
            prefix_same_as_start: false,
            pin_data: false,
            background_purge_on_iterator_cleanup: false,
            readahead_size: 0,
            ignore_range_deletions: false,
        }
    }
}


/// Options that control write operations
#[repr(C)]
pub struct WriteOptions {
    /// If true, the write will be flushed from the operating system
    /// buffer cache (by calling WritableFile::Sync()) before the write
    /// is considered complete.  If this flag is true, writes will be
    /// slower.
    ///
    /// If this flag is false, and the machine crashes, some recent
    /// writes may be lost.  Note that if it is just the process that
    /// crashes (i.e., the machine does not reboot), no writes will be
    /// lost even if sync==false.
    ///
    /// In other words, a DB write with sync==false has similar
    /// crash semantics as the "write()" system call.  A DB write
    /// with sync==true has similar crash semantics to a "write()"
    /// system call followed by "fdatasync()".
    ///
    /// Default: false
    pub sync: bool,

    /// If true, writes will not first go to the write ahead log,
    /// and the write may got lost after a crash.
    pub disableWAL: bool,

    // The option is deprecated. It's not used anymore.
    // timeout_hint_us: u64,
    /// If true and if user is trying to write to column families that don't exist
    /// (they were dropped),  ignore the write (don't return an error). If there
    /// are multiple writes in a WriteBatch, other writes will succeed.
    /// Default: false
    pub ignore_missing_column_families: bool,

    /// If true and we need to wait or sleep for the write request, fails
    /// immediately with Status::Incomplete().
    pub no_slowdown: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            sync: false,
            disableWAL: false,
            // timeout_hint_us: 0,
            ignore_missing_column_families: false,
            no_slowdown: false,
        }
    }
}


/// Options that control flush operations
#[repr(C)]
pub struct FlushOptions {
    /// If true, the flush will wait until the flush is done.
    /// Default: true
    pub wait: bool,
}

impl Default for FlushOptions {
    fn default() -> Self {
        FlushOptions { wait: true }
    }
}



/// CompactionOptions are used in CompactFiles() call.
#[repr(C)]
pub struct CompactionOptions {
    /// Compaction output compression type
    /// Default: snappy
    pub compression: CompressionType,
    /// Compaction will create files of size `output_file_size_limit`.
    /// Default: MAX, which means that compaction will create a single file
    pub output_file_size_limit: u64,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        CompactionOptions {
            compression: CompressionType::SnappyCompression,
            output_file_size_limit: u64::MAX,
        }
    }
}



/// For level based compaction, we can configure if we want to skip/force
/// bottommost level compaction.
#[repr(C)]
pub enum BottommostLevelCompaction {
    /// Skip bottommost level compaction
    Skip,
    /// Only compact bottommost level if there is a compaction filter
    /// This is the default option
    IfHaveCompactionFilter,
    /// Always compact bottommost level
    Force,
}


/// CompactRangeOptions is used by CompactRange() call.
#[repr(C)]
pub struct CompactRangeOptions {
    /// If true, no other compaction will run at the same time as this
    /// manual compaction
    exclusive_manual_compaction: bool,
    /// If true, compacted files will be moved to the minimum level capable
    /// of holding the data or given level (specified non-negative target_level).
    change_level: bool,
    /// If change_level is true and target_level have non-negative value, compacted
    /// files will be moved to target_level.
    target_level: i32,
    /// Compaction outputs will be placed in options.db_paths[target_path_id].
    /// Behavior is undefined if target_path_id is out of range.
    target_path_id: u32,
    /// By default level based compaction will only compact the bottommost level
    /// if there is a compaction filter
    bottommost_level_compaction: BottommostLevelCompaction,
}

impl Default for CompactRangeOptions {
    fn default() -> Self {
        CompactRangeOptions {
            exclusive_manual_compaction: true,
            change_level: false,
            target_level: -1,
            target_path_id: 0,
            bottommost_level_compaction: BottommostLevelCompaction::IfHaveCompactionFilter,
        }
    }
}

/// IngestExternalFileOptions is used by IngestExternalFile()
#[repr(C)]
pub struct IngestExternalFileOptions {
    /// Can be set to true to move the files instead of copying them.
    pub move_files: bool,
    /// If set to false, an ingested file keys could appear in existing snapshots
    /// that where created before the file was ingested.
    pub snapshot_consistency: bool,
    /// If set to false, IngestExternalFile() will fail if the file key range
    /// overlaps with existing keys or tombstones in the DB.
    pub allow_global_seqno: bool,
    /// If set to false and the file key range overlaps with the memtable key range
    /// (memtable flush required), IngestExternalFile will fail.
    pub allow_blocking_flush: bool,
}

impl Default for IngestExternalFileOptions {
    fn default() -> Self {
        IngestExternalFileOptions {
            move_files: false,
            snapshot_consistency: true,
            allow_global_seqno: true,
            allow_blocking_flush: true,
        }
    }
}