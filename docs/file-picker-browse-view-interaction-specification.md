# File Picker and Browse View Interaction Specification

## 1. Scope

This specification applies to both of the following surfaces:

1. the app-level **Browse view**; and
2. the dedicated, reusable **file-picker crate** on which the Browse view is, or should be, substantially based.

Unless a requirement is explicitly limited to one surface, the resulting capability must be available in both surfaces. The requirements describe the required end state rather than implying that every capability is currently absent from both implementations.

For each requirement:

- if a surface already implements the required behavior correctly, preserve it and verify that it satisfies this specification;
- if the corresponding behavior is missing or weaker in the other surface, implement it there;
- if both surfaces implement the behavior independently, consolidate it into the reusable file-picker crate or a lower-level shared component wherever practical; and
- do not remove, weaken, or regress an existing Browse view capability merely to make the two surfaces superficially identical.

An existing implementation is therefore not to be ignored. It is the baseline to verify, preserve, and, where appropriate, migrate into the shared implementation so the other surface can consume the same behavior.

The Browse view and the file-picker crate must remain at behavioral parity for all shared capabilities described here, including navigation, type-to-select, search, bookmarks, file creation, inline naming and renaming, text editing, mouse behavior, context menus, selection, clipboard operations, and filesystem actions.

Shared behavior should be implemented in the reusable file-picker crate, or in lower-level components shared by the crate and the Browse view, wherever practical. The Browse view must not carry an independent, weaker, or behaviorally inconsistent implementation of functionality that belongs in the shared layer.

App-specific behavior may be added by the Browse view, but any intentional difference from the reusable file-picker behavior must be explicit and documented.

## 2. Terminology

To avoid ambiguity, this document uses the following terms:

- **Browse view**: the app-level browsing screen or surface.
- **File picker**: the dedicated reusable file-picker crate and its rendered UI.
- **Tree pane**: the hierarchical directory tree.
- **Browse pane**: the flat file-and-folder listing for the directory currently being displayed. This may also be called the explorer pane in implementation code.
- **Active pane**: whichever of the tree pane or browse pane currently has navigation focus.
- **Text editor**: either the path bar editor or an inline file/folder name editor.
- **Displayed directory**: the directory whose immediate children are shown in the browse pane.
- **Right-click target**: the item or background region under the pointer when a context menu is opened.

Where this specification refers generically to the file picker, the requirement also applies to the corresponding behavior in the app-level Browse view unless stated otherwise.

## 3. General behavioral requirements

Equivalent interactions must behave the same in the Browse view and the file picker:

- The same keyboard and mouse actions must produce the same result.
- The same validation and filesystem-operation rules must apply.
- The same focus, selection, commit, cancellation, and error-handling semantics must apply.
- Platform-specific modifier conventions must be respected consistently.
- Context-menu actions must operate on the right-click target or the applicable selection, not accidentally on a stale keyboard cursor.
- Shared capabilities must expose equivalent visual and interaction states, except where the surrounding app layout necessarily differs.

## 4. Pane layout and title bar

The file-picker surface must have a solid title bar consistent with the title bars used by the Browse view, the Info pane, and the panes on the Convert screen.

The title bar must:

- always open at the file picker's normal dimensions;
- use dimensions dynamically determined from the available terminal size, as it does today;
- not support minimizing;
- support maximizing to the full available terminal area; and
- support restoring the previous normal dimensions.

Clicking the Unicode disclosure/pyramid control in the title bar must toggle between maximized and restored states.

The same maximize/restore behavior should be used wherever the equivalent file-picker surface appears in the app.

## 5. Type-to-select navigation

Type-to-select applies to the active pane.

When the tree pane or browse pane has focus and no modal, context menu, or text editor is active:

1. Typing a sequence of characters must perform incremental name matching within the active pane.
2. Matching directories must be considered before matching files.
3. The first matching directory must receive focus or selection.
4. Only when no directory matches may the first matching file receive focus or selection.

Example:

- Typing `libr` should select a directory named `library`.
- If no matching directory exists, it may select a file such as `library.db`.

The matching algorithm, case behavior, timeout behavior, and cycling behavior must be identical in the Browse view and the file picker. Type-to-select must not intercept input while the path bar or an inline name editor has text focus.

## 6. Tree-pane expansion and collapse

Directory expansion state in the tree pane must be controllable consistently by keyboard and mouse.

For a directory row:

- Activating the disclosure/pyramid control with the mouse must toggle expansion.
- Double-clicking an expanded directory must be able to collapse it.
- Double-clicking a collapsed directory must be able to expand it or navigate according to the established tree interaction model.
- Existing cursor-key behavior for expanding and collapsing directories must continue to work.

The visual state of the disclosure control must accurately reflect the directory's expansion state. Mouse behavior must not contradict the affordance shown by the control.

## 7. Search

Both the Browse view and the file picker must provide a rich, professional search capability.

The implementation should reuse the app's existing Browse search behavior or the same shared search component rather than creating separate search systems.

At minimum, the two surfaces must remain at parity for:

- search invocation;
- query editing;
- result presentation;
- keyboard and mouse navigation;
- focus behavior;
- cancellation and clearing;
- result activation;
- validation and error handling; and
- filesystem scope and filtering semantics.

Any search feature available in one surface but intentionally unavailable in the other must be explicitly documented and justified.

## 8. Bookmarks

Both the Browse view and the file picker must provide polished bookmark support.

Bookmark behavior must be shared or kept at parity across both surfaces, including:

- creating a bookmark for a directory;
- removing a bookmark;
- renaming or relabeling a bookmark, if bookmark labels are supported;
- reordering bookmarks, if ordering is user-controlled;
- navigating to a bookmark;
- representing missing or inaccessible bookmark targets;
- persisting bookmark state; and
- exposing bookmark actions through consistent keyboard, mouse, toolbar, and context-menu interactions where applicable.

Bookmark operations must have clear focus, selection, confirmation, error, and persistence semantics.

## 9. Creating files and folders

Both the Browse view and the file picker must support creating and naming new files and folders.

### 9.1 Toolbar interaction

The toolbar must include a **New** action.

Activating **New** must open a submenu containing:

- **File**
- **Folder**

After the user chooses a type, naming must occur inline in the appropriate picker pane. A separate naming pop-up or modal must not be used.

The inline editor must make the target location visually unambiguous.

### 9.2 Tree-pane target rules

Right-clicking a folder in the tree pane must offer a **New** submenu containing:

- **File**
- **Folder**

The new item must be created inside the folder that was right-clicked.

The **New** submenu must not appear when the right-click target is a file in the tree pane.

### 9.3 Browse-pane target rules

Right-clicking the empty background of the browse pane must offer a **New** submenu containing:

- **File**
- **Folder**

The new item must be created directly inside the displayed directory.

Right-clicking a folder item in the browse pane must not cause **New -> File** or **New -> Folder** to create anything inside that folder. To create an item inside a folder from the browse pane, the user must first open the folder so that it becomes the displayed directory.

Therefore:

- In the tree pane, **New** targets the folder that was right-clicked.
- In the browse pane, **New** targets the directory currently being displayed.

## 10. Context menus

Context menus must behave consistently in the Browse view and the file picker.

### 10.1 Path bar

Right-clicking the path bar must open a standard text-editing context menu containing:

- **Cut**
- **Copy**
- **Paste**

The actions must operate on the current text selection.

Pasting must replace the selected text or insert text at the cursor when no text is selected, allowing the user to navigate to a path copied from elsewhere.

### 10.2 Files and folders

Right-clicking a file or folder in either the tree pane or browse pane must offer the applicable actions:

- **Cut**
- **Copy**
- **Rename**
- **Delete**
- **Duplicate**
- **Open in New Tab**, only when tabbed browsing is implemented
- **Open/Edit with System Default**, for files only

Actions must apply to the right-click target, regardless of the current keyboard cursor or selection, subject to the browse-pane multi-selection rules in Section 11.

Single-item-only actions must be disabled or hidden when they are not applicable.

### 10.3 Browse-pane background

Right-clicking the empty background of the browse pane must offer:

- the **New** submenu described in Section 9.3;
- the **Selection** submenu described in Section 11.2; and
- **Paste**, when the filesystem clipboard contains files or folders that can be pasted.

Pasted items must be created inside the displayed directory.

## 11. Browse-pane selection and bulk operations

Multi-selection is required in the browse pane. It is not required in the tree pane.

### 11.1 Selection scope

Selection commands operate only on the immediate items currently displayed in the browse pane.

They must not recursively select descendants of displayed folders.

### 11.2 Selection submenu

The app-level Browse view already provides these selection commands. That existing behavior must be preserved and treated as the parity baseline for the file picker. If the dedicated file-picker crate or any Browse view path that uses it does not yet expose equivalent behavior, implement or refactor the shared capability so both surfaces do.

The browse-pane background context menu in both surfaces must include a **Selection** submenu containing:

- **Select All**
- **Invert Selection**
- **Deselect All**

A surface that already satisfies this requirement requires verification and regression coverage, not a duplicate implementation. The objective is one consistent capability available through both surfaces, preferably backed by shared code.

These commands must be restricted to the browse pane. They must not be offered for the tree pane, because inversion over a hierarchical tree is ambiguous with respect to visible rows, expanded descendants, and undisplayed filesystem content.

### 11.3 Keyboard selection commands

When the browse pane has focus and no modal, context menu, or inline editor is active:

- `Ctrl+A` selects all displayed items.
- `Esc` deselects all items.
- Existing keyboard and mouse mechanisms for adding or removing individual items from the selection must continue to work.

On platforms with different standard modifiers, the platform convention applies; for example, macOS uses `Command+A`.

### 11.4 Right-click and selection interaction

Bulk-capable actions include:

- **Cut**
- **Copy**
- **Delete**
- **Duplicate**

They must follow these rules:

1. If the user right-clicks an item that is already part of a multi-item selection, the action applies to every selected item.
2. If the user right-clicks an item that is not selected, the existing selection is cleared, the right-clicked item becomes the sole selected item, and the action applies only to that item.
3. Actions that inherently operate on one item only, including **Rename**, **Open/Edit with System Default**, and **Open in New Tab**, must be disabled or hidden when multiple items are selected.

### 11.5 Text-focus precedence

Selection commands must not appear or take precedence while the path bar or an inline name editor has text focus.

In a text editor:

- `Ctrl+A` selects text.
- Cut, Copy, and Paste operate on text.
- Browse-pane selection must remain unchanged unless the established editing workflow explicitly requires otherwise.

## 12. Path bar text editing

The path bar must provide standard text-selection and editing behavior.

It must support:

- `Ctrl+A` to select the entire path;
- double-click to select the entire path;
- `Ctrl+Shift+Left Arrow` to extend the selection by one path or text segment to the left;
- `Ctrl+Shift+Right Arrow` to extend the selection by one path or text segment to the right;
- `Ctrl+X` to cut the selected text;
- `Ctrl+C` to copy the selected text; and
- `Ctrl+V` to replace the selected text with clipboard contents, or insert clipboard contents at the cursor when no text is selected.

Path-segment movement and selection should treat path separators as meaningful boundaries while remaining consistent with the platform's standard text-editing conventions.

## 13. Inline naming and renaming

Inline name editing must be supported in both the tree pane and browse pane.

This applies to:

- naming a newly created file or folder; and
- renaming an existing file or folder.

### 13.1 Delayed-click rename interaction

Existing items must support the platform-standard delayed-click rename interaction:

1. The first click selects the file or folder.
2. After the normal double-click interval has elapsed, a later click on the already selected item's name enters inline rename mode.
3. A rapid double-click retains its existing open-or-navigate behavior and must not enter rename mode.

The implementation must distinguish a deliberate delayed rename click from a double-click without making open/navigation behavior unreliable.

### 13.2 Inline editor behavior

While an inline name editor is active:

- `Ctrl+A` selects the complete file or folder name.
- `Ctrl+Shift+Left Arrow` extends the text selection by one word or name segment to the left.
- `Ctrl+Shift+Right Arrow` extends the text selection by one word or name segment to the right.
- `Ctrl+X` cuts the selected text.
- `Ctrl+C` copies the selected text.
- `Ctrl+V` replaces the selected text with clipboard contents, or inserts clipboard contents at the cursor when no text is selected.
- `Enter` validates and commits the name.
- `Esc` cancels the edit and restores the original name for a rename, or cancels creation for a new item.
- Clicking outside the editor commits or cancels according to the file picker's established editing policy.

The same validation, collision handling, error reporting, commit behavior, and cancellation behavior must apply in the Browse view and the file picker.

## 14. Platform conventions

Keyboard shortcuts and modifier names in this document use `Ctrl` as the generic convention.

The implementation must follow the operating system's corresponding standard modifier conventions where applicable, including `Command` instead of `Ctrl` on macOS.

Mouse timing, double-click timing, clipboard integration, default-editor integration, and text-selection behavior should use platform conventions or platform-provided settings where available.

## 15. Conditional capabilities

### 15.1 Tabbed browsing

**Open in New Tab** must be shown only when tabbed browsing is implemented and available in the current surface.

When tabs are unavailable, the action must be omitted rather than presented as nonfunctional.

### 15.2 System default editor

**Open/Edit with System Default** applies only to files.

It must use the platform's normal mechanism for opening the file with its associated application or editor. It must not be shown for directories.

## 16. Acceptance criteria

The work is complete only when:

- every pre-existing compliant capability has been preserved and covered against regression;
- every capability missing from either surface has been implemented there;
- the Browse view and file picker exhibit equivalent behavior for every shared requirement in this document;
- shared functionality is implemented in the reusable crate or a common lower-level component wherever practical;
- right-click actions operate on the correct target or selection;
- text-editing shortcuts never conflict with browse-pane selection shortcuts;
- tree-pane and browse-pane creation rules target the correct directory;
- rapid double-click and delayed-click rename behavior are reliably distinguished;
- multi-selection and bulk actions are limited to the browse pane;
- search and bookmark behavior are at parity across both surfaces; and
- intentional surface-specific differences are explicit, documented, and tested.
