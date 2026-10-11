//! Loose desktop icons: transparent, event-driven surfaces owning a complete desktop view.

use super::*;
use crate::layout::{CellRect, GridMetrics};
use pecofence_core::DesktopPosition;

impl App {
    pub(super) fn is_desktop_view(&self, fence: FenceId) -> bool {
        fence.is_nil() || self.desktop_windows.values().any(|w| w.id == fence)
    }

    pub(super) fn selected_desktop_items(&self) -> Vec<ItemId> {
        let mut items: Vec<_> = self
            .desktop_windows
            .values()
            .flat_map(|w| w.desktop_selection())
            .collect();
        items.sort_unstable();
        items.dedup();
        items
    }

    pub(super) fn clear_desktop_selection(&self, except: FenceId) {
        for w in self.desktop_windows.values().filter(|w| w.id != except) {
            if !w.desktop_selection().is_empty() {
                w.select_desktop_all(false);
            }
        }
    }

    pub(super) fn desktop_action(&mut self, fence: FenceId, action: DesktopAction) {
        let items = self.selected_desktop_items();
        tracing::info!(?action, count = items.len(), "desktop action");
        match action {
            DesktopAction::SelectAll => {
                for w in self.desktop_windows.values() {
                    w.select_desktop_all(true);
                }
            }
            DesktopAction::Clear => self.clear_desktop_selection(uuid::Uuid::nil()),
            DesktopAction::Copy { cut } if !items.is_empty() => {
                self.clipboard_verb(fence, &items, cut)
            }
            DesktopAction::Delete { permanent } if !items.is_empty() => {
                self.delete_items(fence, &items, permanent)
            }
            DesktopAction::Open => {
                for id in items {
                    self.launch(id);
                }
            }
            DesktopAction::Rename => {
                if let [item] = items[..] {
                    self.begin_item_rename(fence, item);
                }
            }
            DesktopAction::Properties if !items.is_empty() => self.show_properties(fence, &items),
            _ => {}
        }
    }
    pub(super) fn release_to_desktop(&mut self, items: &[ItemId], point: Option<(i32, i32)>) {
        self.release_to_desktop_at(items, point, None);
    }

    pub(super) fn drop_onto_desktop(
        &mut self,
        items: &[ItemId],
        point: Option<(i32, i32)>,
        grab_offset: Option<(f32, f32)>,
        mode: TransferMode,
    ) {
        if mode == TransferMode::Move {
            self.release_to_desktop_at(items, point, grab_offset);
            return;
        }
        let Some(dest) = shell::user_desktop() else {
            return;
        };
        let paths: Vec<_> = self
            .shell_paths_of(items)
            .into_iter()
            .filter(|p| !shell::is_namespace_path(p))
            .collect();
        if paths.is_empty() {
            return;
        }
        if mode == TransferMode::Copy {
            let before = std::fs::read_dir(&dest)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            self.start_fileop(FileOp {
                paths,
                dest: dest.clone(),
                copy: true,
                rename_on_collision: true,
                owner: self.desktop_windows.values().next().map(|w| w.hwnd()),
                then: FileOpThen::Duplicate {
                    to: uuid::Uuid::nil(),
                    dest,
                    before,
                    position: point,
                },
            });
        } else {
            for path in self.create_shortcuts_in(&paths, &dest) {
                self.pending_routes.push(PendingRoute {
                    fence: uuid::Uuid::nil(),
                    path,
                    since: Instant::now(),
                    rename: false,
                    desktop_position: point,
                });
            }
            self.sync_desktop_if_available("desktop shortcut drag");
            self.route_pending_creation();
            self.refresh_all();
            self.schedule_save();
        }
    }

    pub(super) fn release_to_desktop_at(
        &mut self,
        items: &[ItemId],
        point: Option<(i32, i32)>,
        grab_offset: Option<(f32, f32)>,
    ) {
        let (portal, desktop): (Vec<_>, Vec<_>) = items
            .iter()
            .copied()
            .partition(|id| self.state.is_portal_item(*id));
        if !portal.is_empty() {
            self.move_portal_items_to_desktop(&portal, uuid::Uuid::nil(), point);
        }
        let positions = point.and_then(|(x, y)| {
            let work = self
                .state
                .work_areas
                .iter()
                .find(|w| x >= w.left && x < w.right && y >= w.top && y < w.bottom)
                .or_else(|| self.state.work_areas.first())?;
            let m = GridMetrics::for_icon_size(self.state.config.settings.icon_size, 2)
                .with_line_h(self.ctx.chrome.label_line_h());
            let width = (work.right - work.left).max(1) as f32;
            let height = (work.bottom - work.top).max(1) as f32;
            let scale = self
                .desktop_windows
                .get(&work.device_path)
                .map_or(work.scale(), |w| w.desktop_scale());
            let wd = width / scale;
            let hd = height / scale;
            let saved = &self.state.config.layouts[self.state.layout].desktop_items;
            let original: Option<Vec<_>> = desktop
                .iter()
                .map(|id| {
                    let p = saved.get(id)?.as_ref()?;
                    let source = self
                        .state
                        .work_areas
                        .iter()
                        .find(|w| w.device_path == p.monitor)?;
                    Some((
                        (source.left as f32 + p.x * (source.right - source.left) as f32) / scale,
                        (source.top as f32 + p.y * (source.bottom - source.top) as f32) / scale,
                    ))
                })
                .collect();
            let offsets = original
                .map(|points| {
                    let first = points.first().copied().unwrap_or_default();
                    points
                        .into_iter()
                        .map(|(x, y)| (x - first.0, y - first.1))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    let columns = ((wd / (m.cell_w + 8.0)).floor() as usize).clamp(1, 5);
                    (0..desktop.len())
                        .map(|i| {
                            (
                                (i % columns) as f32 * (m.cell_w + 8.0),
                                (i / columns) as f32 * (m.cell_h + 8.0),
                            )
                        })
                        .collect()
                });
            let occupied: Vec<_> = self
                .state
                .loose_items()
                .iter()
                .filter(|it| !desktop.contains(&it.id))
                .filter_map(|it| {
                    let p = saved.get(&it.id)?.as_ref()?;
                    (p.monitor == work.device_path).then_some(CellRect {
                        x: p.x * wd,
                        y: p.y * hd,
                        w: m.cell_w,
                        h: m.cell_h,
                    })
                })
                .collect();
            let grab = grab_offset.unwrap_or((m.cell_w / 2.0, m.icon_top + m.icon / 2.0));
            let cells = place_desktop_group(
                &offsets,
                (
                    (x - work.left) as f32 / scale - grab.0,
                    (y - work.top) as f32 / scale - grab.1,
                ),
                (wd, hd),
                (m.cell_w, m.cell_h),
                self.state.config.settings.desktop_view.align_grid,
                &occupied,
            )?;
            Some(
                desktop
                    .iter()
                    .zip(cells)
                    .map(|(id, cell)| {
                        (
                            *id,
                            DesktopPosition {
                                monitor: work.device_path.clone(),
                                x: cell.x / wd,
                                y: cell.y / hd,
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        });
        if self.state.release_items(&desktop, None) > 0 {
            if let Some(positions) = positions {
                for (id, p) in positions {
                    self.state.config.layouts[self.state.layout]
                        .desktop_items
                        .insert(id, Some(p));
                }
            }
            self.refresh_all();
            self.schedule_save();
            self.push_workspace_summary();
        }
    }

    pub(super) fn refresh_desktop(&mut self) {
        self.refresh_desktop_layout(self.state.config.settings.desktop_view.auto_arrange);
    }

    pub(super) fn arrange_desktop(&mut self) {
        self.refresh_desktop_layout(true);
        self.schedule_save();
    }

    pub(super) fn align_desktop_grid(&mut self) {
        let m = GridMetrics::for_icon_size(self.state.config.settings.icon_size, 2)
            .with_line_h(self.ctx.chrome.label_line_h());
        for p in self.state.config.layouts[self.state.layout]
            .desktop_items
            .values_mut()
            .flatten()
        {
            if let Some(work) = self
                .state
                .work_areas
                .iter()
                .find(|w| w.device_path == p.monitor)
            {
                let scale = self
                    .desktop_windows
                    .get(&work.device_path)
                    .map_or(work.scale(), |w| w.desktop_scale());
                let wd = (work.right - work.left) as f32 / scale;
                let hd = (work.bottom - work.top) as f32 / scale;
                p.x = (8.0
                    + ((p.x * wd - 8.0) / (m.cell_w + 8.0)).round().max(0.0) * (m.cell_w + 8.0))
                    .min((wd - m.cell_w).max(0.0))
                    / wd;
                p.y = (8.0
                    + ((p.y * hd - 8.0) / (m.cell_h + 8.0)).round().max(0.0) * (m.cell_h + 8.0))
                    .min((hd - m.cell_h).max(0.0))
                    / hd;
            }
        }
        self.state.mark_dirty();
        self.refresh_desktop();
    }

    fn refresh_desktop_layout(&mut self, arrange: bool) {
        if !self.state.config.settings.hide_real_icons || self.no_hide_icons {
            self.desktop_windows.clear();
            return;
        }
        let works = self.state.work_areas.clone();
        let Some(first) = works.first() else {
            return;
        };
        self.desktop_windows
            .retain(|key, _| works.iter().any(|w| &w.device_path == key));
        let placements = self.state.config.layouts[self.state.layout]
            .desktop_items
            .clone();
        let loose = self.state.loose_items();
        let mut groups: HashMap<String, Vec<ItemView>> = HashMap::new();
        for item in loose {
            let monitor = placements
                .get(&item.id)
                .and_then(Option::as_ref)
                .map(|p| p.monitor.as_str())
                .filter(|id| works.iter().any(|w| w.device_path == *id))
                .unwrap_or(&first.device_path);
            let path = PathBuf::from(item.key.as_path().unwrap_or(""));
            let (_, icon_only) = crate::icons::icon_key_for(&path, item.is_folder, item.mtime);
            groups
                .entry(monitor.to_string())
                .or_default()
                .push(ItemView::new(
                    item.id,
                    path,
                    item.display_name.clone(),
                    item.is_folder,
                    item.icon_key.clone(),
                    icon_only,
                    item.mtime,
                    item.size,
                ));
        }
        let fence_rects: Vec<_> = self
            .state
            .host_fences()
            .iter()
            .map(|f| self.state.fence_px_rect(f))
            .collect();
        let m = GridMetrics::for_icon_size(self.state.config.settings.icon_size, 2)
            .with_line_h(self.ctx.chrome.label_line_h());
        let mut saved = Vec::new();
        for work in &works {
            let items = groups.remove(&work.device_path).unwrap_or_default();
            let bounds = RECT {
                left: work.left,
                top: work.top,
                right: work.right,
                bottom: work.bottom,
            };
            if !self.desktop_windows.contains_key(&work.device_path) {
                match FenceWindow::create_desktop(
                    &self.ctx,
                    uuid::Uuid::new_v4(),
                    bounds,
                    self.state.config.settings.icon_size,
                ) {
                    Ok(w) => {
                        self.desktop_windows.insert(work.device_path.clone(), w);
                    }
                    Err(error) => {
                        tracing::error!(%error, "desktop icon surface failed");
                        continue;
                    }
                }
            }
            let scale = self.desktop_windows[&work.device_path].desktop_scale();
            let width = (work.right - work.left).max(1) as f32;
            let height = (work.bottom - work.top).max(1) as f32;
            let wd = width / scale;
            let hd = height / scale;
            let mut cells = Vec::<CellRect>::new();
            // Reserve saved positions before finding spots for newly arrived icons.
            let reserved: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    if arrange {
                        return None;
                    }
                    let p = placements.get(&item.id)?.as_ref()?;
                    Some(CellRect {
                        x: (p.x * wd).clamp(0.0, (wd - m.cell_w).max(0.0)),
                        y: (p.y * hd).clamp(0.0, (hd - m.cell_h).max(0.0)),
                        w: m.cell_w,
                        h: m.cell_h,
                    })
                })
                .collect();
            let overlaps = |a: CellRect, b: CellRect| {
                a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
            };
            for item in &items {
                let existing = (!arrange)
                    .then(|| placements.get(&item.id).and_then(Option::as_ref))
                    .flatten();
                let mut cell = existing.map(|p| CellRect {
                    x: (p.x * wd).clamp(0.0, (wd - m.cell_w).max(0.0)),
                    y: (p.y * hd).clamp(0.0, (hd - m.cell_h).max(0.0)),
                    w: m.cell_w,
                    h: m.cell_h,
                });
                if cell.is_some_and(|c| cells.iter().any(|placed| overlaps(c, *placed))) {
                    cell = None;
                }
                if cell.is_none() {
                    'place: for col in 0..((wd / (m.cell_w + 8.0)).floor() as usize).max(1) {
                        for row in 0..((hd / (m.cell_h + 8.0)).floor() as usize).max(1) {
                            let candidate = CellRect {
                                x: 8.0 + col as f32 * (m.cell_w + 8.0),
                                y: 8.0 + row as f32 * (m.cell_h + 8.0),
                                w: m.cell_w,
                                h: m.cell_h,
                            };
                            let on_fence = fence_rects.iter().any(|r| {
                                let x = work.left as f32 + candidate.x * scale;
                                let y = work.top as f32 + candidate.y * scale;
                                x < r.right as f32
                                    && (r.left as f32) < x + candidate.w * scale
                                    && y < r.bottom as f32
                                    && (r.top as f32) < y + candidate.h * scale
                            });
                            if !on_fence
                                && !cells
                                    .iter()
                                    .chain(reserved.iter())
                                    .any(|c| overlaps(candidate, *c))
                            {
                                cell = Some(candidate);
                                break 'place;
                            }
                        }
                    }
                }
                let cell = cell.unwrap_or(CellRect {
                    x: 8.0,
                    y: 8.0,
                    w: m.cell_w,
                    h: m.cell_h,
                });
                saved.push((
                    item.id,
                    DesktopPosition {
                        monitor: work.device_path.clone(),
                        x: cell.x / wd,
                        y: cell.y / hd,
                    },
                ));
                cells.push(cell);
            }
            if let Some(w) = self.desktop_windows.get(&work.device_path) {
                if w.rect() != bounds {
                    w.set_bounds(bounds);
                }
                w.set_icon_size(self.state.config.settings.icon_size);
                w.set_desktop_items(items, cells);
                w.show(false);
            }
        }
        for (id, position) in saved {
            let entry = self.state.config.layouts[self.state.layout]
                .desktop_items
                .entry(id)
                .or_default();
            if entry.as_ref() != Some(&position) {
                *entry = Some(position);
                self.state.mark_dirty();
            }
        }
        if let Some(anchor) = self.anchor.borrow_mut().as_mut() {
            anchor.reanchor("desktop icon surfaces");
        }
    }
}

/// Translate a desktop selection as one group; snap the anchor only, so relative positions
/// survive a drag. If the group cannot fit, the caller lets the regular layout place it.
fn place_desktop_group(
    offsets: &[(f32, f32)],
    anchor: (f32, f32),
    area: (f32, f32),
    size: (f32, f32),
    snap: bool,
    occupied: &[CellRect],
) -> Option<Vec<CellRect>> {
    if offsets.is_empty() {
        return Some(Vec::new());
    }
    let min_x = offsets.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let min_y = offsets.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_x = offsets
        .iter()
        .map(|p| p.0 + size.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = offsets
        .iter()
        .map(|p| p.1 + size.1)
        .fold(f32::NEG_INFINITY, f32::max);
    if max_x - min_x > area.0 || max_y - min_y > area.1 {
        return None;
    }
    let pitch = (size.0 + 8.0, size.1 + 8.0);
    let at = if snap {
        (
            8.0 + ((anchor.0 - 8.0) / pitch.0).round() * pitch.0,
            8.0 + ((anchor.1 - 8.0) / pitch.1).round() * pitch.1,
        )
    } else {
        anchor
    };
    let at = (
        at.0.clamp(-min_x, area.0 - max_x),
        at.1.clamp(-min_y, area.1 - max_y),
    );
    let cells_at = |(x, y): (f32, f32)| {
        offsets
            .iter()
            .map(|(dx, dy)| CellRect {
                x: x + dx,
                y: y + dy,
                w: size.0,
                h: size.1,
            })
            .collect::<Vec<_>>()
    };
    let fits = |cells: &[CellRect]| {
        cells.iter().all(|a| {
            occupied.iter().all(|b| {
                a.x >= b.x + b.w || b.x >= a.x + a.w || a.y >= b.y + b.h || b.y >= a.y + a.h
            })
        })
    };
    let cells = cells_at(at);
    if fits(&cells) {
        return Some(cells);
    }
    let mut candidates = Vec::new();
    for col in 0..((area.0 / pitch.0).ceil() as usize).max(1) {
        for row in 0..((area.1 / pitch.1).ceil() as usize).max(1) {
            let p = (8.0 + col as f32 * pitch.0, 8.0 + row as f32 * pitch.1);
            if p.0 >= -min_x && p.1 >= -min_y && p.0 + max_x <= area.0 && p.1 + max_y <= area.1 {
                candidates.push(p);
            }
        }
    }
    candidates.sort_by(|a, b| {
        let distance = |p: &(f32, f32)| (p.0 - at.0).powi(2) + (p.1 - at.1).powi(2);
        distance(a).total_cmp(&distance(b))
    });
    candidates
        .into_iter()
        .map(cells_at)
        .find(|cells| fits(cells))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_drag_preserves_offsets_and_stays_inside_work_area() {
        let offsets = [(0.0, 0.0), (-88.0, 96.0), (88.0, 96.0)];
        let cells = place_desktop_group(
            &offsets,
            (990.0, 790.0),
            (1000.0, 800.0),
            (80.0, 88.0),
            true,
            &[],
        )
        .unwrap();
        for (cell, offset) in cells.iter().zip(offsets) {
            assert_eq!((cell.x - cells[0].x, cell.y - cells[0].y), offset);
            assert!(
                cell.x >= 0.0
                    && cell.y >= 0.0
                    && cell.x + cell.w <= 1000.0
                    && cell.y + cell.h <= 800.0
            );
        }
    }

    #[test]
    fn occupied_drop_moves_the_whole_selection_to_a_free_spot() {
        let occupied = [CellRect {
            x: 8.0,
            y: 8.0,
            w: 80.0,
            h: 88.0,
        }];
        let cells = place_desktop_group(
            &[(0.0, 0.0), (88.0, 0.0)],
            (8.0, 8.0),
            (800.0, 600.0),
            (80.0, 88.0),
            true,
            &occupied,
        )
        .unwrap();
        assert_eq!(cells[1].x - cells[0].x, 88.0);
        assert!(cells[0].x >= 88.0 || cells[0].y >= 96.0);
    }
}
