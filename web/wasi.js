/*
 * A WASI preview-1 host, hand-written, with an in-memory filesystem.
 *
 * # Why this exists rather than a dependency
 *
 * The repository's whole claim is an empty dependency manifest, and a
 * playground that demonstrates it by pulling a WASI polyfill off a CDN would be
 * making the opposite point. So the host is written here, and the page fetches
 * nothing at runtime — the same rule the dashboard page follows.
 *
 * It is also much smaller than a general one, because zql needs a specific and
 * small slice of the interface: open a file, read it, seek in it, stat it, list
 * a directory, ask the clock. Everything else is stubbed to the error WASI
 * defines for "not supported", which is what a real host would return anyway
 * for, say, a socket call this module never makes.
 *
 * # The filesystem
 *
 * Files are `Uint8Array`s in a tree of plain objects, mounted at /demo. That is
 * the whole illusion: `std::fs::File::open` in Rust becomes `path_open` here,
 * `read` becomes `fd_read`, and the SQLite pager walks a real b-tree over bytes
 * that happen to live in a JavaScript array rather than on a disk. The engine
 * cannot tell, which is the point — the page runs the shipped reader, not a
 * browser-flavoured imitation of it.
 */
'use strict';

// The subset of WASI errno this host can return.
const E = { SUCCESS: 0, BADF: 8, EXIST: 20, INVAL: 28, ISDIR: 31, NOENT: 44, NOSYS: 52, NOTDIR: 54, PERM: 63 };

// filetype: the two kinds this host has.
const FILETYPE_DIRECTORY = 3;
const FILETYPE_REGULAR = 4;

// oflags
const O_CREAT = 1 << 0;
const O_DIRECTORY = 1 << 1;
const O_TRUNC = 1 << 3;

class Directory {
  constructor(entries = {}) { this.entries = entries; }
  get filetype() { return FILETYPE_DIRECTORY; }
  get size() { return 0; }
}

class File {
  constructor(bytes) { this.bytes = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes); }
  get filetype() { return FILETYPE_REGULAR; }
  get size() { return this.bytes.length; }
}

class WASI {
  /**
   * @param {object} tree  nested plain object; values are Uint8Array (file) or
   *                       object (directory). Mounted at /demo.
   * @param {(s:string)=>void} onStderr  where the module's stderr goes.
   */
  constructor(tree, onStderr = () => {}) {
    this.root = buildTree(tree);
    this.onStderr = onStderr;
    this.memory = null;

    // fd 0/1/2 are the standard streams; 3 is the preopened /demo directory,
    // which is what makes `path_open` on "/demo/simple.db" resolvable at all.
    this.fds = [
      { kind: 'stdio' }, { kind: 'stdio' }, { kind: 'stdio' },
      { kind: 'preopen', name: '/demo', node: this.root, offset: 0 },
    ];
    this.decoder = new TextDecoder();
    this.encoder = new TextEncoder();
  }

  bind(instance) { this.memory = instance.exports.memory; }

  get view() { return new DataView(this.memory.buffer); }
  get bytes() { return new Uint8Array(this.memory.buffer); }

  readString(ptr, len) { return this.decoder.decode(this.bytes.subarray(ptr, ptr + len)); }

  /** Resolves a path against the mount, returning a node or null. */
  resolve(path) {
    const parts = path.split('/').filter((p) => p && p !== '.' && p !== '/demo');
    let node = this.root;
    for (const part of parts) {
      if (!(node instanceof Directory)) return null;
      node = node.entries[part];
      if (node === undefined) return null;
    }
    return node;
  }

  /** The import object a `WebAssembly.instantiate` call needs. */
  imports() {
    const self = this;
    const ok = () => E.SUCCESS;

    return {
      wasi_snapshot_preview1: {
        // ------------------------------------------------------------ files
        path_open(dirfd, _dirflags, pathPtr, pathLen, oflags, _rb, _ri, _fdflags, fdPtr) {
          const path = self.readString(pathPtr, pathLen);
          const node = self.resolve(path);
          if (node === undefined || node === null) {
            // Creating is refused rather than faked: zql never opens a file for
            // writing, and a host that silently allowed it would let a
            // regression through that the real binary would not have.
            return (oflags & O_CREAT) ? E.PERM : E.NOENT;
          }
          if ((oflags & O_DIRECTORY) && !(node instanceof Directory)) return E.NOTDIR;
          if (oflags & O_TRUNC) return E.PERM;

          const fd = self.fds.length;
          self.fds.push({ kind: node instanceof Directory ? 'dir' : 'file', node, offset: 0 });
          self.view.setUint32(fdPtr, fd, true);
          return E.SUCCESS;
        },

        fd_close(fd) {
          if (!self.fds[fd]) return E.BADF;
          self.fds[fd] = null;
          return E.SUCCESS;
        },

        fd_read(fd, iovsPtr, iovsLen, nreadPtr) {
          const h = self.fds[fd];
          if (!h) return E.BADF;
          if (h.kind === 'stdio') { self.view.setUint32(nreadPtr, 0, true); return E.SUCCESS; }
          if (h.kind !== 'file') return E.ISDIR;

          let read = 0;
          const view = self.view;
          for (let i = 0; i < iovsLen; i++) {
            const base = view.getUint32(iovsPtr + i * 8, true);
            const len = view.getUint32(iovsPtr + i * 8 + 4, true);
            const chunk = h.node.bytes.subarray(h.offset, h.offset + len);
            if (chunk.length === 0) break;
            self.bytes.set(chunk, base);
            h.offset += chunk.length;
            read += chunk.length;
          }
          self.view.setUint32(nreadPtr, read, true);
          return E.SUCCESS;
        },

        fd_write(fd, iovsPtr, iovsLen, nwrittenPtr) {
          const view = self.view;
          let written = 0, text = '';
          for (let i = 0; i < iovsLen; i++) {
            const base = view.getUint32(iovsPtr + i * 8, true);
            const len = view.getUint32(iovsPtr + i * 8 + 4, true);
            text += self.readString(base, len);
            written += len;
          }
          // Only the standard streams accept writes. Anything else is the
          // engine trying to modify a file, which is a bug worth surfacing
          // rather than absorbing.
          if (fd !== 1 && fd !== 2) return E.PERM;
          if (text) self.onStderr(text);
          self.view.setUint32(nwrittenPtr, written, true);
          return E.SUCCESS;
        },

        // `filedelta` is an i64, so it arrives as a BigInt — WebAssembly's i64
        // maps to BigInt and mixing it with a Number throws. The whole seek is
        // done in BigInt and narrowed once, at the end.
        fd_seek(fd, delta, whence, newOffsetPtr) {
          const h = self.fds[fd];
          if (!h || h.kind !== 'file') return E.BADF;
          const size = BigInt(h.node.size);
          const base = whence === 0 ? 0n : whence === 1 ? BigInt(h.offset) : size;
          let next = base + BigInt(delta);
          if (next < 0n) return E.INVAL;
          if (next > size) next = size;
          h.offset = Number(next);
          self.view.setBigUint64(newOffsetPtr, next, true);
          return E.SUCCESS;
        },

        fd_tell(fd, ptr) {
          const h = self.fds[fd];
          if (!h) return E.BADF;
          self.view.setBigUint64(ptr, BigInt(h.offset || 0), true);
          return E.SUCCESS;
        },

        // ------------------------------------------------------------- stat
        fd_fdstat_get(fd, ptr) {
          const h = self.fds[fd];
          if (!h) return E.BADF;
          const type = h.kind === 'file' ? FILETYPE_REGULAR
            : h.kind === 'stdio' ? 2 /* character device */ : FILETYPE_DIRECTORY;
          const view = self.view;
          view.setUint8(ptr, type);
          view.setUint16(ptr + 2, 0, true);
          view.setBigUint64(ptr + 8, 0xffffffffffffffffn, true);
          view.setBigUint64(ptr + 16, 0xffffffffffffffffn, true);
          return E.SUCCESS;
        },

        fd_filestat_get(fd, ptr) {
          const h = self.fds[fd];
          if (!h || !h.node) return E.BADF;
          writeFilestat(self.view, ptr, h.node);
          return E.SUCCESS;
        },

        path_filestat_get(_dirfd, _flags, pathPtr, pathLen, ptr) {
          const node = self.resolve(self.readString(pathPtr, pathLen));
          if (!node) return E.NOENT;
          writeFilestat(self.view, ptr, node);
          return E.SUCCESS;
        },

        // -------------------------------------------------------- directory
        //
        // This is the one Node's own WASI would not do for us on Windows, and
        // without it `files()` silently walks nothing — the source swallows a
        // failed read_dir by design, so a broken listing looks like an empty
        // directory rather than an error.
        fd_readdir(fd, bufPtr, bufLen, cookie, sizePtr) {
          const h = self.fds[fd];
          if (!h || !(h.node instanceof Directory)) return E.BADF;

          const names = Object.keys(h.node.entries);
          const bytes = self.bytes;
          const view = self.view;
          let offset = 0;

          for (let i = Number(cookie); i < names.length; i++) {
            const name = names[i];
            const encoded = self.encoder.encode(name);
            const entry = h.node.entries[name];
            // dirent is 24 bytes, then the name. A truncated final entry is
            // legal and tells the caller to come back with a larger buffer.
            if (offset + 24 + encoded.length > bufLen) break;
            view.setBigUint64(bufPtr + offset, BigInt(i + 1), true);       // d_next
            view.setBigUint64(bufPtr + offset + 8, BigInt(i + 1), true);   // d_ino
            view.setUint32(bufPtr + offset + 16, encoded.length, true);    // d_namlen
            view.setUint8(bufPtr + offset + 20, entry.filetype);           // d_type
            bytes.set(encoded, bufPtr + offset + 24);
            offset += 24 + encoded.length;
          }
          view.setUint32(sizePtr, offset, true);
          return E.SUCCESS;
        },

        // --------------------------------------------------------- preopens
        fd_prestat_get(fd, ptr) {
          const h = self.fds[fd];
          if (!h || h.kind !== 'preopen') return E.BADF;
          const view = self.view;
          view.setUint8(ptr, 0);                                    // tag: dir
          view.setUint32(ptr + 4, self.encoder.encode(h.name).length, true);
          return E.SUCCESS;
        },

        fd_prestat_dir_name(fd, ptr, len) {
          const h = self.fds[fd];
          if (!h || h.kind !== 'preopen') return E.BADF;
          const encoded = self.encoder.encode(h.name);
          if (encoded.length > len) return E.INVAL;
          self.bytes.set(encoded, ptr);
          return E.SUCCESS;
        },

        // ------------------------------------------------------------ misc
        clock_time_get(_id, _precision, ptr) {
          // Milliseconds from the page, in nanoseconds as WASI wants them. The
          // timing the playground reports comes from here.
          self.view.setBigUint64(ptr, BigInt(Math.round(Date.now() * 1e6)), true);
          return E.SUCCESS;
        },

        random_get(ptr, len) {
          crypto.getRandomValues(self.bytes.subarray(ptr, ptr + len));
          return E.SUCCESS;
        },

        environ_sizes_get(countPtr, sizePtr) {
          const view = self.view;
          view.setUint32(countPtr, 0, true);
          view.setUint32(sizePtr, 0, true);
          return E.SUCCESS;
        },
        environ_get: ok,

        args_sizes_get(countPtr, sizePtr) {
          const view = self.view;
          view.setUint32(countPtr, 0, true);
          view.setUint32(sizePtr, 0, true);
          return E.SUCCESS;
        },
        args_get: ok,

        proc_exit(code) { throw new Error(`the module called proc_exit(${code})`); },

        // Everything zql never calls. Returning NOSYS rather than omitting them
        // matters: a missing import is a link error that stops the module
        // instantiating at all, which would be a confusing way to discover that
        // something reached for a socket.
        fd_advise: () => E.NOSYS,
        fd_allocate: () => E.PERM,
        fd_datasync: () => E.SUCCESS,
        fd_sync: () => E.SUCCESS,
        fd_fdstat_set_flags: () => E.SUCCESS,
        fd_fdstat_set_rights: () => E.SUCCESS,
        fd_filestat_set_size: () => E.PERM,
        fd_filestat_set_times: () => E.PERM,
        fd_pread: () => E.NOSYS,
        fd_pwrite: () => E.PERM,
        fd_renumber: () => E.NOSYS,
        path_create_directory: () => E.PERM,
        path_filestat_set_times: () => E.PERM,
        path_link: () => E.PERM,
        path_readlink: () => E.INVAL,
        path_remove_directory: () => E.PERM,
        path_rename: () => E.PERM,
        path_symlink: () => E.PERM,
        path_unlink_file: () => E.PERM,
        poll_oneoff: () => E.NOSYS,
        sched_yield: () => E.SUCCESS,
        sock_accept: () => E.NOSYS,
        sock_recv: () => E.NOSYS,
        sock_send: () => E.NOSYS,
        sock_shutdown: () => E.NOSYS,
        proc_raise: () => E.NOSYS,
      },
    };
  }
}

function writeFilestat(view, ptr, node) {
  view.setBigUint64(ptr, 0n, true);                       // dev
  view.setBigUint64(ptr + 8, 1n, true);                   // ino
  view.setUint8(ptr + 16, node.filetype);
  view.setBigUint64(ptr + 24, 1n, true);                  // nlink
  view.setBigUint64(ptr + 32, BigInt(node.size), true);   // size
  const now = BigInt(Math.round(Date.now() * 1e6));
  view.setBigUint64(ptr + 40, now, true);                 // atim
  view.setBigUint64(ptr + 48, now, true);                 // mtim
  view.setBigUint64(ptr + 56, now, true);                 // ctim
}

function buildTree(tree) {
  const dir = new Directory();
  for (const [name, value] of Object.entries(tree)) {
    dir.entries[name] = (value instanceof Uint8Array || value instanceof ArrayBuffer)
      ? new File(value)
      : buildTree(value);
  }
  return dir;
}

export { WASI, File, Directory };
