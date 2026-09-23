package com.lerdr.app.session

import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.di.AppEntryPoint
import com.lerdr.core.designsystem.components.LerdrLoadingIndicator
import com.lerdr.core.designsystem.components.LerdrSegmentedControl
import com.lerdr.core.designsystem.theme.LerdrExtendedColors
import com.lerdr.core.designsystem.theme.LerdrTextStyles
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Files mode — workspace tree, file preview, git status + diff
 * (docs/04 §Files; oracle `WorkspaceInspector` parity). On a phone the
 * oracle's sidebar+preview split becomes list→detail: the preview pane
 * swaps into the content area while a selection is live.
 */
@Composable
fun FilesScreen(
    paneId: String,
    onOpenFeed: () -> Unit,
    onOpenTerminal: () -> Unit,
    onBack: () -> Unit,
    onSelectTab: (String) -> Unit = {},
) {
    val appContext = LocalContext.current.applicationContext
    val viewModel: FilesViewModel = viewModel(key = "files:$paneId") {
        val entryPoint = EntryPointAccessors.fromApplication(appContext, AppEntryPoint::class.java)
        FilesViewModel(paneId, entryPoint.sessionRepository())
    }
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    FilesContent(
        uiState = uiState,
        onOpenFeed = onOpenFeed,
        onOpenTerminal = onOpenTerminal,
        onBack = onBack,
        onSelectTab = onSelectTab,
        tabsPaneId = paneId,
        onSelectSection = viewModel::selectSection,
        onOpenDir = viewModel::openDir,
        onShowFile = viewModel::showFile,
        onShowDiff = viewModel::showDiff,
        onClosePreview = viewModel::closePreview,
        onFilterChange = viewModel::onFilterChange,
        onRefresh = viewModel::loadWorkspace,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FilesContent(
    uiState: FilesUiState,
    onOpenFeed: () -> Unit,
    onOpenTerminal: () -> Unit,
    onBack: () -> Unit,
    onSelectTab: (String) -> Unit = {},
    tabsPaneId: String? = null,
    onSelectSection: (FilesSection) -> Unit,
    onOpenDir: (String) -> Unit,
    onShowFile: (String) -> Unit,
    onShowDiff: (String) -> Unit,
    onClosePreview: () -> Unit,
    onFilterChange: (String) -> Unit,
    onRefresh: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val colors = LerdrTheme.extendedColors

    // In-place preview: Back returns to the listing before leaving the mode.
    BackHandler(enabled = uiState.previewVisible, onBack = onClosePreview)

    Scaffold(
        topBar = {
            SessionTopBar(
                title = uiState.title.ifEmpty { uiState.paneId.substringAfter("::") },
                breadcrumb = uiState.breadcrumb,
                statusLabel = uiState.statusLabel.ifEmpty {
                    if (uiState.connected) "connected" else "offline"
                },
                statusColor = if (uiState.connected) colors.live else colors.idle,
                mode = SessionMode.FILES,
                onSelectMode = { mode ->
                    when (mode) {
                        SessionMode.FEED -> onOpenFeed()
                        SessionMode.TERMINAL -> onOpenTerminal()
                        SessionMode.FILES -> Unit
                    }
                },
                onBack = onBack,
                provider = uiState.provider,
                active = uiState.connected,
                tabsPaneId = tabsPaneId,
                onSelectTab = { onSelectTab(it.paneId) },
                actions = listOf(
                    SessionBarAction(
                        label = "Refresh",
                        icon = Icons.Default.Refresh,
                        onClick = onRefresh,
                    ),
                ),
            )
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
            if (uiState.previewVisible) {
                PreviewHeader(
                    path = uiState.selectedPath,
                    kind = uiState.previewKind,
                    previewFile = uiState.previewFile,
                    onClose = onClosePreview,
                )
                PreviewBody(uiState = uiState, modifier = Modifier.weight(1f))
            } else {
                BrowserToolbar(
                    uiState = uiState,
                    onSelectSection = onSelectSection,
                    onFilterChange = onFilterChange,
                )
                Box(modifier = Modifier.weight(1f)) {
                    BrowserBody(
                        uiState = uiState,
                        onOpenDir = onOpenDir,
                        onShowFile = onShowFile,
                        onShowDiff = onShowDiff,
                        onRefresh = onRefresh,
                    )
                }
            }
        }
    }
}

// ── toolbar ──────────────────────────────────────────────────────────────

@Composable
private fun BrowserToolbar(
    uiState: FilesUiState,
    onSelectSection: (FilesSection) -> Unit,
    onFilterChange: (String) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    Column(modifier = Modifier.padding(horizontal = spacing.medium)) {
        LerdrSegmentedControl(
            options = FilesSection.entries.map { section ->
                if (section == FilesSection.CHANGES && (uiState.git?.files?.size ?: 0) > 0) {
                    "${section.label} (${uiState.git?.files?.size})"
                } else {
                    section.label
                }
            },
            selectedIndex = uiState.section.ordinal,
            onSelect = { onSelectSection(FilesSection.entries[it]) },
            modifier = Modifier.fillMaxWidth(),
        )
        // docs/04 Details: the cwd chip — tap copies the workspace root.
        uiState.tree?.root?.takeIf { it.isNotEmpty() }?.let { root ->
            MetaChip(
                label = root,
                description = "Working directory",
                onClick = {
                    scope.launch {
                        clipboard.setClipEntry(
                            ClipEntry(
                                android.content.ClipData.newPlainText(
                                    "Workspace path",
                                    root,
                                ),
                            ),
                        )
                    }
                },
                modifier = Modifier
                    .padding(top = spacing.small)
                    .testTag("files:cwd"),
            )
        }
        uiState.git?.takeIf { it.available }?.let { git ->
            Row(
                horizontalArrangement = Arrangement.spacedBy(spacing.small),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(vertical = spacing.small),
            ) {
                if (git.branch.isNotEmpty()) {
                    MetaChip(label = git.branch, description = "Git branch")
                }
                MetaChip(
                    label = if (git.ahead != null && git.behind != null) {
                        "↑${git.ahead} ↓${git.behind}"
                    } else {
                        "No upstream"
                    },
                    description = "Commits relative to the configured upstream",
                )
            }
        }
        OutlinedTextField(
            value = uiState.filter,
            onValueChange = onFilterChange,
            placeholder = { Text("Filter ${uiState.section.label.lowercase()}…") },
            leadingIcon = {
                Icon(Icons.Default.Search, contentDescription = null)
            },
            singleLine = true,
            shape = MaterialTheme.shapes.extraLarge,
            modifier = Modifier
                .fillMaxWidth()
                .padding(bottom = spacing.small),
        )
    }
}

@Composable
private fun MetaChip(
    label: String,
    description: String,
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
        shape = CircleShape,
        modifier = modifier
            .semantics { contentDescription = description }
            .then(
                if (onClick != null) {
                    Modifier.clickable(
                        onClickLabel = "Copy $description",
                        role = Role.Button,
                        onClick = onClick,
                    )
                } else {
                    Modifier
                },
            ),
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        )
    }
}

// ── browser body ─────────────────────────────────────────────────────────

@Composable
private fun BrowserBody(
    uiState: FilesUiState,
    onOpenDir: (String) -> Unit,
    onShowFile: (String) -> Unit,
    onShowDiff: (String) -> Unit,
    onRefresh: () -> Unit,
) {
    when {
        uiState.loading -> MessageRow("Reading workspace…", loading = true)
        uiState.workspaceError != null -> MessageRow(
            text = uiState.workspaceError,
            isError = true,
            actionLabel = "Retry",
            onAction = onRefresh,
        )
        !uiState.connected && uiState.tree == null -> MessageRow(
            text = "Relay is offline — reconnect to browse this workspace.",
        )
        uiState.section == FilesSection.FILES -> TreeList(
            uiState = uiState,
            onOpenDir = onOpenDir,
            onShowFile = onShowFile,
        )
        else -> ChangesList(
            uiState = uiState,
            onShowDiff = onShowDiff,
        )
    }
}

/** The drill-down file tree — breadcrumbs on top, one directory per page. */
@Composable
private fun TreeList(
    uiState: FilesUiState,
    onOpenDir: (String) -> Unit,
    onShowFile: (String) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    val entries = uiState.tree?.entries.orEmpty()
    val needle = uiState.filter.trim().lowercase()
    val filtering = needle.isNotEmpty()
    val visible = remember(entries, uiState.currentDir, needle) {
        if (filtering) {
            entries.filter { it.path.lowercase().contains(needle) }
        } else {
            childrenOf(entries, uiState.currentDir)
        }
    }
    val crumbs = remember(uiState.rootLabel, uiState.currentDir) {
        breadcrumbsOf(uiState.rootLabel, uiState.currentDir)
    }
    Column {
        // Path breadcrumbs — every crumb before the last navigates back up.
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = spacing.medium, vertical = spacing.extraSmall),
        ) {
            crumbs.forEachIndexed { index, crumb ->
                if (index > 0) {
                    Text(
                        "/",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                val last = index == crumbs.lastIndex
                TextButton(
                    onClick = { onOpenDir(crumb.dir) },
                    enabled = !last,
                    contentPadding = PaddingValues(
                        horizontal = spacing.extraSmall,
                        vertical = 0.dp,
                    ),
                ) {
                    Text(
                        crumb.label,
                        style = MaterialTheme.typography.labelMedium,
                        maxLines = 1,
                        color = if (last) {
                            MaterialTheme.colorScheme.onSurface
                        } else {
                            MaterialTheme.colorScheme.primary
                        },
                    )
                }
            }
        }
        LazyColumn(modifier = Modifier.fillMaxSize()) {
            if (uiState.tree?.truncated == true) {
                item(key = "truncated") {
                    NoticeRow("File list limited to the first 4,000 entries.")
                }
            }
            items(visible, key = { it.path }) { entry ->
                TreeEntryRow(
                    entry = entry,
                    selected = entry.path == uiState.selectedPath &&
                        uiState.previewKind == FilesPreviewKind.FILE,
                    showFullPath = filtering,
                    onClick = {
                        if (entry.isDirectory) onOpenDir(entry.path) else onShowFile(entry.path)
                    },
                )
            }
            if (visible.isEmpty()) {
                item(key = "empty") {
                    MessageRow(
                        text = if (filtering) {
                            "No matching files."
                        } else {
                            "This folder is empty."
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun TreeEntryRow(
    entry: WorkspaceTreeEntry,
    selected: Boolean,
    showFullPath: Boolean,
    onClick: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        onClick = onClick,
        color = if (selected) {
            MaterialTheme.colorScheme.secondaryContainer
        } else {
            Color.Transparent
        },
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(
                horizontal = spacing.medium,
                vertical = spacing.small,
            ),
        ) {
            Icon(
                if (entry.isDirectory) Icons.Default.Folder else Icons.Default.Description,
                contentDescription = if (entry.isDirectory) "Folder" else "File",
                tint = if (entry.isDirectory) {
                    MaterialTheme.colorScheme.primary
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.size(20.dp),
            )
            Spacer(Modifier.width(spacing.small))
            Column(Modifier.weight(1f)) {
                Text(
                    entry.name,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (showFullPath) {
                    Text(
                        entry.path,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (entry.isDirectory) {
                Icon(
                    Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                entry.size?.let { size ->
                    Text(
                        formatFileSize(size),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/** The Changes tab — porcelain rows with status badges. */
@Composable
private fun ChangesList(
    uiState: FilesUiState,
    onShowDiff: (String) -> Unit,
) {
    val git = uiState.git
    val needle = uiState.filter.trim().lowercase()
    val visible = remember(git?.files, needle) {
        git?.files.orEmpty().filter {
            needle.isEmpty() || "${it.path} ${it.originalPath}".lowercase().contains(needle)
        }
    }
    when {
        git == null || !git.available -> MessageRow(
            text = uiState.gitReason ?: "Git status is unavailable for this workspace.",
        )
        else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
            if (git.truncated) {
                item(key = "truncated") {
                    NoticeRow("Changed-file list is truncated.")
                }
            }
            items(visible, key = { it.path }) { file ->
                ChangedFileRow(
                    file = file,
                    selected = file.path == uiState.selectedPath &&
                        uiState.previewKind == FilesPreviewKind.DIFF,
                    onClick = { onShowDiff(file.path) },
                )
            }
            if (visible.isEmpty()) {
                item(key = "empty") {
                    MessageRow(
                        text = if (needle.isNotEmpty()) {
                            "No matching changes."
                        } else {
                            "Working tree clean."
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ChangedFileRow(
    file: WorkspaceGitFile,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        onClick = onClick,
        color = if (selected) {
            MaterialTheme.colorScheme.secondaryContainer
        } else {
            Color.Transparent
        },
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(
                horizontal = spacing.medium,
                vertical = spacing.small,
            ),
        ) {
            StatusBadge(status = file.status)
            Spacer(Modifier.width(spacing.small))
            Column(Modifier.weight(1f)) {
                Text(
                    file.path,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (file.originalPath.isNotEmpty()) {
                    Text(
                        "renamed from ${file.originalPath}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun StatusBadge(status: String) {
    val colors = LerdrTheme.extendedColors
    val label = gitStatusLabel(status)
    val color = when (label) {
        "New", "Added" -> colors.working
        "Modified" -> MaterialTheme.colorScheme.primary
        "Deleted" -> MaterialTheme.colorScheme.error
        "Renamed" -> colors.chat
        else -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(
        color = color.copy(alpha = 0.18f),
        contentColor = color,
        shape = MaterialTheme.shapes.small,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.padding(
                horizontal = LerdrTheme.spacing.small,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
        )
    }
}

// ── preview pane ─────────────────────────────────────────────────────────

@Composable
private fun PreviewHeader(
    path: String,
    kind: FilesPreviewKind,
    previewFile: WorkspaceFilePreview?,
    onClose: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = spacing.extraSmall, end = spacing.medium),
        ) {
            IconButton(onClick = onClose) {
                Icon(
                    Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back to file list",
                )
            }
            Column(Modifier.padding(vertical = spacing.small)) {
                Text(
                    path,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    buildString {
                        append(
                            if (kind == FilesPreviewKind.DIFF) "Unified diff" else "File preview",
                        )
                        previewFile?.let {
                            append(" · ${it.mediaType} · ${formatFileSize(it.size)}")
                        }
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun PreviewBody(uiState: FilesUiState, modifier: Modifier = Modifier) {
    Box(modifier = modifier.fillMaxWidth()) {
        when {
            uiState.previewLoading -> MessageRow("Loading ${uiState.selectedPath}…", loading = true)
            uiState.previewError != null -> MessageRow(
                text = uiState.previewError,
                isError = true,
            )
            uiState.previewDiff != null -> DiffPreview(uiState.previewDiff)
            uiState.previewFile != null -> FilePreview(uiState.previewFile)
        }
    }
}

@Composable
private fun FilePreview(preview: WorkspaceFilePreview) {
    when (preview.kind) {
        WorkspacePreviewKind.TEXT -> TextPreview(preview)
        WorkspacePreviewKind.IMAGE -> ImagePreview(preview)
    }
}

/** Bounded text — monospace, vertically scrollable, selectable. */
@Composable
private fun TextPreview(preview: WorkspaceFilePreview) {
    SelectionContainer {
        Text(
            preview.text,
            style = LerdrTextStyles.code,
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(LerdrTheme.spacing.medium),
        )
    }
}

/** `data:<media>;base64,…` → decoded bitmap, off the main thread. */
@Composable
private fun ImagePreview(preview: WorkspaceFilePreview) {
    val bitmap by produceState<ImageBitmap?>(null, preview.dataUrl) {
        value = withContext(Dispatchers.Default) { decodeDataUrlBitmap(preview.dataUrl) }
    }
    val image = bitmap
    when {
        image != null -> Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(LerdrTheme.spacing.medium),
        ) {
            androidx.compose.foundation.Image(
                bitmap = image,
                contentDescription = "Preview of ${preview.path}",
                contentScale = ContentScale.FillWidth,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        preview.dataUrl.isEmpty() -> MessageRow(
            text = "This image preview carried no data.",
            isError = true,
        )
        else -> MessageRow("Loading ${preview.path}…", loading = true)
    }
}

private fun decodeDataUrlBitmap(dataUrl: String): ImageBitmap? {
    val encoded = dataUrl.substringAfter("base64,", "")
    if (encoded.isEmpty()) return null
    return runCatching {
        val bytes = android.util.Base64.decode(encoded, android.util.Base64.DEFAULT)
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
    }.getOrNull()
}

/** Unified diff — per-line tone colors, oracle `diffLineTone` parity. */
@Composable
private fun DiffPreview(preview: WorkspaceGitDiff) {
    val lines = remember(preview.diff) { preview.diff.split('\n') }
    if (lines.size <= 1 && lines.firstOrNull().isNullOrEmpty()) {
        MessageRow("No text diff for this file.")
        return
    }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = LerdrTheme.spacing.small),
    ) {
        itemsIndexed(lines) { _, line ->
            DiffLine(line)
        }
    }
}

@Composable
private fun DiffLine(line: String) {
    val colors = LerdrTheme.extendedColors
    val tone = diffLineToneOf(line)
    val (background, foreground) = diffToneColors(tone, colors)
    Surface(color = background, modifier = Modifier.fillMaxWidth()) {
        Text(
            // Blank lines still need a row height in the diff.
            line.ifEmpty { " " },
            style = LerdrTextStyles.code,
            color = foreground,
            modifier = Modifier.padding(horizontal = LerdrTheme.spacing.medium),
        )
    }
}

@Composable
private fun diffToneColors(
    tone: DiffLineTone,
    colors: LerdrExtendedColors,
): Pair<Color, Color> = when (tone) {
    DiffLineTone.ADDITION -> colors.workingContainer to colors.onWorkingContainer
    DiffLineTone.DELETION ->
        MaterialTheme.colorScheme.errorContainer to MaterialTheme.colorScheme.onErrorContainer
    DiffLineTone.HUNK ->
        MaterialTheme.colorScheme.surfaceContainerHigh to MaterialTheme.colorScheme.primary
    DiffLineTone.META, DiffLineTone.FILE ->
        Color.Transparent to MaterialTheme.colorScheme.onSurfaceVariant
    DiffLineTone.NOTE -> Color.Transparent to MaterialTheme.colorScheme.onSurfaceVariant
    DiffLineTone.CONTEXT -> Color.Transparent to MaterialTheme.colorScheme.onSurface
}

// ── shared bits ──────────────────────────────────────────────────────────

@Composable
private fun NoticeRow(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .fillMaxWidth()
            .padding(
                horizontal = LerdrTheme.spacing.medium,
                vertical = LerdrTheme.spacing.extraSmall,
            ),
    )
}

/** Full-area status line — loading spinner, error tint, optional action. */
@Composable
private fun MessageRow(
    text: String,
    loading: Boolean = false,
    isError: Boolean = false,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
) {
    val spacing = LerdrTheme.spacing
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
        modifier = Modifier
            .fillMaxSize()
            .padding(spacing.large),
    ) {
        if (loading) {
            LerdrLoadingIndicator(modifier = Modifier.size(32.dp))
            Spacer(Modifier.size(spacing.medium))
        }
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            color = if (isError) {
                MaterialTheme.colorScheme.error
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
        if (actionLabel != null && onAction != null) {
            Spacer(Modifier.size(spacing.small))
            TextButton(onClick = onAction) { Text(actionLabel) }
        }
    }
}

/** Human size — bytes → B/KB/MB (oracle shows ceil-KB; this stays honest). */
internal fun formatFileSize(bytes: Long): String = when {
    bytes < 1024 -> "$bytes B"
    bytes < 1024 * 1024 -> "${(bytes + 1023) / 1024} KB"
    else -> "${(bytes + 1024 * 1024 - 1) / (1024 * 1024)} MB"
}

@PreviewLightDark
@Composable
private fun FilesContentPreview() {
    LerdrTheme {
        FilesContent(
            uiState = FilesUiState(
                paneId = "sd::%1",
                title = "claude",
                provider = "claude",
                breadcrumb = "lerdr · main · sd",
                connected = true,
                loading = false,
                rootLabel = "lerdr",
                tree = WorkspaceTree(
                    root = "/home/u/lerdr",
                    entries = listOf(
                        WorkspaceTreeEntry("app", "app", WorkspaceEntryKind.DIRECTORY),
                        WorkspaceTreeEntry("relay", "relay", WorkspaceEntryKind.DIRECTORY),
                        WorkspaceTreeEntry("AGENTS.md", "AGENTS.md", WorkspaceEntryKind.FILE, 2048),
                        WorkspaceTreeEntry("README.md", "README.md", WorkspaceEntryKind.FILE, 512),
                    ),
                ),
                git = WorkspaceGitStatus(
                    available = true,
                    branch = "main",
                    ahead = 2,
                    behind = 0,
                ),
            ),
            onOpenFeed = {},
            onOpenTerminal = {},
            onBack = {},
            onSelectSection = {},
            onOpenDir = {},
            onShowFile = {},
            onShowDiff = {},
            onClosePreview = {},
            onFilterChange = {},
            onRefresh = {},
        )
    }
}
