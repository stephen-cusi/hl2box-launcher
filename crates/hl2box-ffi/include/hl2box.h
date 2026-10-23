/* hl2box launcher C interface.
 *
 * For native embedders: the Android launcher (Kotlin, via JNI) and any
 * other process that wants the same addon / mount / identity / launch
 * logic the desktop GUI uses. Build the hl2box-ffi crate as a static or
 * shared library and link it.
 *
 * Conventions:
 *   - Strings are UTF-8 and NUL-terminated.
 *   - Functions returning char* hand over ownership: free with
 *     hl2box_free. NULL means failure; call hl2box_last_error.
 *   - Functions returning int: 0 ok, -1 failure (see hl2box_last_error).
 *   - Thread safe: state is per-call; errors are per-thread.
 */

#ifndef HL2BOX_H
#define HL2BOX_H

#ifdef __cplusplus
extern "C" {
#endif

#ifdef _WIN32
#define HL2BOX_EXPORT __declspec(dllexport)
#else
#define HL2BOX_EXPORT __attribute__((visibility("default")))
#endif

/* General ---------------------------------------------------------------- */

HL2BOX_EXPORT const char *hl2box_version(void);
HL2BOX_EXPORT void hl2box_free(void *ptr);
HL2BOX_EXPORT const char *hl2box_last_error(void);

/* Identity --------------------------------------------------------------- */

/* Returns identity JSON: {"name":..., "steam_id":"<decimal>", "avatar":...} */
HL2BOX_EXPORT char *hl2box_identity_load(const char *engine_root,
                                         const char *mod_dir);
HL2BOX_EXPORT int hl2box_identity_save(const char *engine_root,
                                       const char *mod_dir,
                                       const char *identity_json);
HL2BOX_EXPORT int hl2box_identity_set_avatar(const char *engine_root,
                                             const char *mod_dir,
                                             const char *image_path);

/* SteamID ---------------------------------------------------------------- */

HL2BOX_EXPORT char *hl2box_steamid_random(void);
/* Returns {"id64":"...","steam2":"STEAM_0:Y:Z","uform":"[U:1:N]",
 *          "account_id":N} or NULL. */
HL2BOX_EXPORT char *hl2box_steamid_parse(const char *input);

/* Addons ----------------------------------------------------------------- */

/* JSON array of {"name","kind":"Folder|Gma","enabled","disk_size",
 * "title","author","file_count"}. */
HL2BOX_EXPORT char *hl2box_addons_list(const char *engine_root,
                                       const char *mod_dir);
HL2BOX_EXPORT int hl2box_addon_set_enabled(const char *engine_root,
                                           const char *mod_dir,
                                           const char *name,
                                           int enabled);
/* Returns the installed addon name. */
HL2BOX_EXPORT char *hl2box_addon_import(const char *engine_root,
                                        const char *mod_dir,
                                        const char *source);
HL2BOX_EXPORT int hl2box_addon_remove(const char *engine_root,
                                      const char *mod_dir,
                                      const char *name);
HL2BOX_EXPORT char *hl2box_gma_info(const char *gma_path);

/* Mounts ----------------------------------------------------------------- */

/* JSON array of {"dir","vpks":[...],"mounted":bool,"managed":bool}. */
HL2BOX_EXPORT char *hl2box_mounts_list(const char *engine_root,
                                       const char *mod_dir);
/* selected_json: JSON array of directory names to mount. */
HL2BOX_EXPORT int hl2box_mounts_apply(const char *engine_root,
                                      const char *mod_dir,
                                      const char *selected_json);

/* Launch ----------------------------------------------------------------- */

HL2BOX_EXPORT char *hl2box_launch_command_line(const char *engine_root,
                                               const char *mod_dir,
                                               const char *extra_args,
                                               const char *identity_json);
/* Returns the started process id, or -1. */
HL2BOX_EXPORT int hl2box_launch_start(const char *engine_root,
                                      const char *mod_dir,
                                      const char *extra_args,
                                      const char *identity_json);

/* Logs ------------------------------------------------------------------- */

/* {"lines":[...]} with the tail of the given log file. */
HL2BOX_EXPORT char *hl2box_log_tail(const char *path, unsigned max_bytes);

/* Workshop --------------------------------------------------------------- */

HL2BOX_EXPORT char *hl2box_workshop_details(const char *id_or_url);
HL2BOX_EXPORT char *hl2box_workshop_download(const char *engine_root,
                                             const char *mod_dir,
                                             const char *id_or_url);

#ifdef __cplusplus
}
#endif

#endif /* HL2BOX_H */
