/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let statusCheckInterval;
let logCheckInterval;
let durationUpdateInterval;
let buildCompleted = false;
let currentEvaluation = null;
let lastLogLength = 0;
let buildIds = [];
let scrollToBottomButton = null;
let buildsData = [];
let currentBuildId = null;
let activeStreamReader = null;
let streamingBuildId = null;
let initialLogsFetched = false;

// Get variables from global scope
window.baseUrl = window.baseUrl || window.location.origin;
const evaluationId = window.location.pathname.split('/').pop();

// Function to format time ago
function formatTimeAgo(dateString) {
  if (!dateString) return 'never';

  const date = new Date(dateString);
  const now = new Date();
  const diffMs = now - date;
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);

  if (diffDays > 0) {
    return `${diffDays} day${diffDays > 1 ? 's' : ''} ago`;
  } else if (diffHours > 0) {
    return `${diffHours} hour${diffHours > 1 ? 's' : ''} ago`;
  } else if (diffMins > 0) {
    return `${diffMins} minute${diffMins > 1 ? 's' : ''} ago`;
  } else {
    return 'just now';
  }
}

// Function to convert ANSI escape sequences to HTML
function convertAnsiToHtml(text) {
  if (!text || typeof text !== 'string') return '';

  return text
    // Handle ANSI sequences with proper escape character
    .replace(/\u001b\[0m/g, '</span>')
    .replace(/\u001b\[31;1m/g, '<span style="color: #ef4444; font-weight: bold;">')
    .replace(/\u001b\[31m/g, '<span style="color: #ef4444;">')
    .replace(/\u001b\[32m/g, '<span style="color: #22c55e;">')
    .replace(/\u001b\[33m/g, '<span style="color: #eab308;">')
    .replace(/\u001b\[34m/g, '<span style="color: #3b82f6;">')
    .replace(/\u001b\[35;1m/g, '<span style="color: #a855f7; font-weight: bold;">')
    .replace(/\u001b\[35m/g, '<span style="color: #a855f7;">')
    .replace(/\u001b\[36m/g, '<span style="color: #06b6d4;">')
    .replace(/\u001b\[37m/g, '<span style="color: #f8fafc;">')
    .replace(/\u001b\[1m/g, '<span style="font-weight: bold;">')
    // Handle ANSI sequences without escape character (malformed)
    .replace(/\[0m/g, '</span>')
    .replace(/\[31;1m/g, '<span style="color: #ef4444; font-weight: bold;">')
    .replace(/\[31m/g, '<span style="color: #ef4444;">')
    .replace(/\[32m/g, '<span style="color: #22c55e;">')
    .replace(/\[33m/g, '<span style="color: #eab308;">')
    .replace(/\[34m/g, '<span style="color: #3b82f6;">')
    .replace(/\[35;1m/g, '<span style="color: #a855f7; font-weight: bold;">')
    .replace(/\[35m/g, '<span style="color: #a855f7;">')
    .replace(/\[36m/g, '<span style="color: #06b6d4;">')
    .replace(/\[37m/g, '<span style="color: #f8fafc;">')
    .replace(/\[1m/g, '<span style="font-weight: bold;">')
    // Remove any remaining escape sequences (both proper and malformed)
    .replace(/\u001b\[[0-9;]*m/g, '')
    .replace(/\[[0-9;]*m/g, '');
}

async function checkBuildStatus() {
  try {
    const response = await fetch(`${window.baseUrl}/api/v1/evals/${evaluationId}`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/json",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });

    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        const evaluation = data.message;
        currentEvaluation = evaluation; // Store for duration updates
        updateBuildStatus(evaluation.status);

        // Update trigger time display
        updateTriggerTime(evaluation.created_at);

        // Update duration - use updated_at for completed builds if available
        const endTime = (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted')
          ? evaluation.updated_at || evaluation.created_at
          : evaluation.created_at;
        updateDuration(evaluation.created_at, evaluation.status, endTime);

        // Start duration timer if not already running and evaluation is active
        if (!durationUpdateInterval && (evaluation.status === 'Running' || evaluation.status === 'Building' || evaluation.status === 'Evaluating' || evaluation.status === 'Queued')) {
          durationUpdateInterval = setInterval(() => {
            if (currentEvaluation) {
              updateDuration(currentEvaluation.created_at, currentEvaluation.status, currentEvaluation.updated_at);
            }
          }, 1000); // Update every second
        }

        // Display evaluation error if it exists
        displayEvaluationError(evaluation.error);

        // Get builds for this evaluation (this will also update the sidebar)
        await fetchBuilds();

        // Stop polling if build is completed, but fetch final builds and logs
        if (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted') {
          clearInterval(statusCheckInterval);
          clearInterval(logCheckInterval);
          clearInterval(durationUpdateInterval);
          durationUpdateInterval = null;
          buildCompleted = true;

          // Fetch final builds and logs when evaluation is complete
          await fetchBuilds();
          await updateLogs();

          return evaluation.status;
        }
      }
    }
  } catch (error) {
    console.error("Error checking build status:", error);
  }
  return null;
}

function updateBuildStatus(status) {
  const statusIcons = document.querySelectorAll('.status-icon');
  const statusTexts = document.querySelectorAll('.status-text');
  const abortButton = document.getElementById('abortButton');

  statusIcons.forEach(icon => {
    icon.className = 'material-icons status-icon';
    if (status === 'Completed') {
      icon.classList.add('green');
      icon.textContent = 'check_circle';
    } else if (status === 'Failed' || status === 'Aborted') {
      icon.classList.add('red');
      icon.textContent = 'cancel';
    } else {
      icon.className = 'loader status-icon';
    }
  });

  statusTexts.forEach(text => {
    text.textContent = status;
  });

  // Update the build tab icon based on evaluation status
  const buildTabIcon = document.querySelector('#build-tab .build-status-icon');
  if (buildTabIcon) {
    if (status === 'Completed') {
      buildTabIcon.className = 'material-icons build-status-icon green';
      buildTabIcon.textContent = 'check_circle';
    } else if (status === 'Failed' || status === 'Aborted') {
      buildTabIcon.className = 'material-icons build-status-icon red';
      buildTabIcon.textContent = 'cancel';
    } else {
      buildTabIcon.className = 'loader build-status-icon';
      buildTabIcon.textContent = '';
    }
  }

  // Show/hide abort button based on status
  if (abortButton) {
    const showAbortButton = status === 'Building' || status === 'Evaluating' || status === 'Queued' || status === 'Running';
    abortButton.style.display = showAbortButton ? 'inline-block' : 'none';
  }

  // Update page title status indicator
  const titleStatusIcon = document.querySelector('.webkit-middle .material-icons, .webkit-middle .loader');
  if (titleStatusIcon) {
    if (status === 'Completed') {
      titleStatusIcon.className = 'material-icons center-image f-s-28px green';
      titleStatusIcon.textContent = 'check_circle';
    } else if (status === 'Failed' || status === 'Aborted') {
      titleStatusIcon.className = 'material-icons center-image f-s-28px red';
      titleStatusIcon.textContent = 'cancel';
    } else {
      titleStatusIcon.className = 'loader';
      titleStatusIcon.textContent = '';
    }
  }

  // Clean up loading build items when evaluation fails
  if (status === 'Failed' || status === 'Aborted') {
    const buildsContainer = document.getElementById('builds-list');
    if (buildsContainer) {
      const loadingItems = buildsContainer.querySelectorAll('.build-item.loading');
      loadingItems.forEach(item => item.remove());
    }
  }
}

function updateDuration(createdAt, status, endTime = null) {
  const durationDisplay = document.getElementById('duration-display');
  if (!durationDisplay || !createdAt) return;

  // Helper function to parse timestamps with timezone handling
  function parseTimestamp(timestamp) {
    if (!timestamp) return null;

    if (timestamp.includes('T') && !timestamp.includes('Z') && !timestamp.includes('+')) {
      // If it's an ISO string without timezone info, assume it's UTC
      return new Date(timestamp + 'Z');
    } else {
      // Otherwise, parse as provided
      return new Date(timestamp);
    }
  }

  const startTime = parseTimestamp(createdAt);
  if (!startTime) return;

  let durationMs;
  const isCompleted = status === 'Completed' || status === 'Failed' || status === 'Aborted';

  if (isCompleted && endTime) {
    // For completed builds, calculate duration from start to end
    const completedTime = parseTimestamp(endTime);
    if (completedTime) {
      durationMs = completedTime.getTime() - startTime.getTime();
    } else {
      durationMs = new Date().getTime() - startTime.getTime();
    }
  } else {
    // For running builds, calculate duration from start to now
    durationMs = new Date().getTime() - startTime.getTime();
  }

  // Only show duration if it's positive (prevents negative durations from timezone issues)
  if (durationMs < 0) {
    console.warn('Negative duration detected, possible timezone issue:', {
      createdAt,
      endTime,
      startTime: startTime.toISOString(),
      now: new Date().toISOString(),
      durationMs
    });
    durationDisplay.textContent = '0:00';
    return;
  }

  // Format duration as HH:MM:SS or MM:SS
  const totalSeconds = Math.floor(durationMs / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  let formattedDuration;
  if (hours > 0) {
    formattedDuration = `${hours}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
  } else {
    formattedDuration = `${minutes}:${seconds.toString().padStart(2, '0')}`;
  }

  durationDisplay.textContent = formattedDuration;

  // Stop updating duration for completed evaluations
  if (isCompleted) {
    return;
  }
}

function updateTriggerTime(createdAt) {
  const triggerTimeDisplay = document.getElementById('trigger-time-display');
  if (!triggerTimeDisplay || !createdAt) return;

  const timeAgo = formatTimeAgo(createdAt);
  triggerTimeDisplay.textContent = `Triggered via schedule ${timeAgo}`;
}

function displayEvaluationError(error) {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;

  if (error) {
    // Clear existing content and show error
    logContainer.innerHTML = '';

    const errorWrapper = document.createElement('div');
    errorWrapper.className = 'evaluation-error';
    errorWrapper.style.cssText = `
      padding: 1.25rem;
      margin: 0;
      border: 1px solid #ff4444;
      border-radius: 8px;
      background: linear-gradient(135deg, #1a0e0e 0%, #2d1414 100%);
      border-left: 4px solid #ff4444;
      box-shadow: 0 4px 12px rgba(255, 68, 68, 0.15);
    `;

    const errorTitle = document.createElement('div');
    errorTitle.className = 'line';
    errorTitle.style.cssText = `
      color: #ff6b6b;
      font-weight: 600;
      margin-bottom: 1rem;
      display: flex;
      align-items: center;
      font-size: 1.1rem;
      padding-left: 0;
    `;
    errorTitle.textContent = 'Evaluation Error';
    errorWrapper.appendChild(errorTitle);

    const errorContent = document.createElement('div');
    errorContent.className = 'line';
    errorContent.style.cssText = `
      color: #ffcccc;
      white-space: pre-wrap;
      font-family: 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace;
      background-color: var(--quaternary, #050708);
      padding: 1rem;
      border-radius: 6px;
      border: 1px solid #3d1f1f;
      overflow-x: auto;
      line-height: 1.5;
      font-size: 0.9rem;
      padding-left: 1rem;
    `;
    errorContent.textContent = error;
    errorWrapper.appendChild(errorContent);

    const errorHint = document.createElement('div');
    errorHint.className = 'line';
    errorHint.style.cssText = `
      color: var(--secondary, #abb0b4);
      font-size: 0.85rem;
      margin-top: 1rem;
      font-style: italic;
      opacity: 0.8;
      padding-left: 0;
    `;
    errorHint.textContent = 'This error occurred during the evaluation phase. Please check your project configuration and try again.';
    errorWrapper.appendChild(errorHint);

    logContainer.appendChild(errorWrapper);

    lastLogLength = 0; // Reset log counter since we cleared the container
  }
}

async function abortBuild() {
  try {
    const response = await fetch(`${window.baseUrl}/api/v1/evals/${evaluationId}/abort`, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/json",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      }
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        // Force a status check to update UI
        await checkBuildStatus();
      } else {
        alert('Failed to abort build: ' + data.error);
      }
    } else {
      const data = await response.json().catch(() => ({}));
      alert('Failed to abort build: ' + (data.error || 'Unknown error'));
    }
  } catch (error) {
    console.error("Error aborting build:", error);
    alert('Error aborting build: ' + error.message);
  }
}

async function fetchBuilds() {
  try {
    console.log(`Fetching builds for evaluation ${evaluationId}`);
    console.log('About to make request to:', `${window.baseUrl}/api/v1/evals/${evaluationId}/builds`);
    const response = await fetch(`${window.baseUrl}/api/v1/evals/${evaluationId}/builds`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/json",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });

    if (response.ok) {
      const data = await response.json();
      console.log('Builds response:', data);
      if (!data.error) {
        buildsData = data.message;
        buildIds = data.message.map(build => build.id);
        console.log(`Found ${buildIds.length} builds:`, buildIds);

        // Update the builds sidebar
        updateBuildsSidebar();

        // Set "all" as current if none selected
        if (!currentBuildId) {
          currentBuildId = 'all';
        }

        // Don't call updateLogs here since it's handled in initializePage
      } else {
        console.error('Error in builds response:', data.error);
      }
    } else {
      console.error('Failed to fetch builds, status:', response.status);
    }
  } catch (error) {
    console.error("Error fetching builds:", error);
  }
}

function updateBuildsSidebar() {
  const buildsContainer = document.getElementById('builds-list');
  if (!buildsContainer) return;

  // Remove all existing dynamic items (loading placeholders and build items)
  const existingDynamicItems = buildsContainer.querySelectorAll('.build-item');
  existingDynamicItems.forEach(item => item.remove());

  // Update builds count display in header
  updateBuildsDisplay();

  if (buildsData.length === 0) {
    // Check if evaluation has failed - if so, don't show loading indicator
    const statusElements = document.querySelectorAll('.status-text');
    let evaluationFailed = false;
    for (let element of statusElements) {
      const status = element.textContent.toLowerCase();
      if (status.includes('failed') || status.includes('aborted')) {
        evaluationFailed = true;
        break;
      }
    }

    if (!evaluationFailed) {
      // Only show loading indicator if evaluation is still running
      const noBuildsItem = document.createElement('div');
      noBuildsItem.className = 'build-item loading';
      noBuildsItem.innerHTML = `
        <div class="loader-small"></div>
        <span>No builds found</span>
      `;
      buildsContainer.appendChild(noBuildsItem);
    }
    // If evaluation failed, don't show any additional items - just keep "All Builds"
    return;
  }


  // Sort builds by status priority: Running, Queued, Failed, Completed
  const sortedBuilds = [...buildsData].sort((a, b) => {
    const statusOrder = {
      'running': 0,
      'queued': 1,
      'pending': 1,
      'failed': 2,
      'aborted': 2,
      'completed': 3,
      'success': 3
    };

    const statusA = (a.status || '').toLowerCase();
    const statusB = (b.status || '').toLowerCase();

    const orderA = statusOrder[statusA] !== undefined ? statusOrder[statusA] : 4;
    const orderB = statusOrder[statusB] !== undefined ? statusOrder[statusB] : 4;

    return orderA - orderB;
  });

  // Add individual build items
  sortedBuilds.forEach((build, index) => {
    const buildItem = createBuildItem(build, index);
    buildsContainer.appendChild(buildItem);
  });

  // Set first build as selected if none is selected (use sorted order)
  if (sortedBuilds.length > 0 && !currentBuildId) {
    selectBuild(sortedBuilds[0].id);
  }

  // Update the log title/status icon for the currently selected build
  if (currentBuildId) {
    updateLogTitle(currentBuildId);
  }
}

function updateBuildsDisplay() {
  const buildsDisplay = document.getElementById('builds-display');
  if (!buildsDisplay) return;

  const total = buildsData.length;
  const completed = buildsData.filter(b => b.status?.toLowerCase() === 'completed').length;
  const failed = buildsData.filter(b => b.status?.toLowerCase() === 'failed').length;

  // Always use format: "Total | Completed in green | Failed in red"
  buildsDisplay.innerHTML = `
    ${total} |
    <span style="color: #22c55e;">${completed}</span> |
    <span style="color: #ef4444;">${failed}</span>
  `;
}

function createBuildItem(build, index) {
  const buildItem = document.createElement('div');
  buildItem.className = `build-item ${currentBuildId === build.id ? 'active' : ''}`;
  buildItem.setAttribute('data-build-id', build.id);

  // Determine status class and icon (using Material Icons like the evaluation title)
  let statusClass = 'pending';
  let statusIconHtml = '';
  let statusText = build.status || 'Pending';

  switch (build.status?.toLowerCase()) {
    case 'completed':
    case 'success':
      statusClass = 'completed';
      statusIconHtml = '<span class="material-icons build-status-icon green">check_circle</span>';
      statusText = 'Completed';
      break;
    case 'failed':
    case 'error':
      statusClass = 'failed';
      statusIconHtml = '<span class="material-icons build-status-icon red">cancel</span>';
      statusText = 'Failed';
      break;
    case 'aborted':
      statusClass = 'failed';
      statusIconHtml = '<span class="material-icons build-status-icon red">cancel</span>';
      statusText = 'Aborted';
      break;
    case 'running':
    case 'building':
      statusClass = 'running';
      statusIconHtml = '<div class="loader build-status-icon"></div>';
      statusText = 'Running';
      break;
    case 'queued':
    case 'pending':
    default:
      statusClass = 'pending';
      statusIconHtml = '<div class="loader build-status-icon"></div>';
      statusText = 'Queued';
      break;
  }

  // Format build name - use evaluation target or fallback
  let rawBuildName = build.evaluation_target || build.name || `Build ${index + 1}`;

  // Clean up build name: remove hash and .drv ending
  let buildName = rawBuildName;
  if (typeof rawBuildName === 'string') {
    // If it's a store path, extract just the package name
    if (rawBuildName.startsWith('/nix/store/')) {
      // Format: /nix/store/hash-package-name.drv or /nix/store/hash-package-name
      const parts = rawBuildName.split('/').pop(); // Get the last part after /
      if (parts) {
        // Remove hash (first 32 characters + dash) and .drv ending
        buildName = parts.replace(/^[a-z0-9]{32}-/, '').replace(/\.drv$/, '');
      }
    }
    // If it contains a hash pattern, remove it
    else if (rawBuildName.includes('-') && /^[a-z0-9]{32}-/.test(rawBuildName)) {
      buildName = rawBuildName.replace(/^[a-z0-9]{32}-/, '').replace(/\.drv$/, '');
    }
  }

  // Format duration if available
  let duration = '';
  if (build.created_at) {
    const startTime = new Date(build.created_at);
    const endTime = build.updated_at ? new Date(build.updated_at) : new Date();
    const durationMs = endTime.getTime() - startTime.getTime();
    const totalSeconds = Math.floor(durationMs / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    duration = `${minutes}:${seconds.toString().padStart(2, '0')}`;
  }

  buildItem.innerHTML = `
    ${statusIconHtml}
    <div class="build-info">
      <div class="build-name" title="${buildName}">${buildName}</div>
      <div class="build-details">
        <span>${duration}</span>
        <span class="build-status ${statusClass}">${statusText}</span>
      </div>
    </div>
  `;

  // Add click handler to switch builds
  buildItem.addEventListener('click', () => {
    selectBuild(build.id);
  });

  return buildItem;
}


function selectBuild(buildId) {
  // Update current build
  currentBuildId = buildId;

  // Update active state in sidebar
  document.querySelectorAll('.build-item').forEach(item => {
    item.classList.remove('active');
    if (item.getAttribute('data-build-id') === buildId) {
      item.classList.add('active');
    }
  });

  // Update log title with build name and architecture
  updateLogTitle(buildId);

  // Clear existing logs and reset stream state for the new build
  const logContainer = document.querySelector('.details-content');
  if (logContainer) {
    logContainer.innerHTML = '<div class="line" style="color: #666; font-style: italic;">Loading build logs...</div>';
    lastLogLength = 0; // Reset log counter
    initialLogsFetched = false; // Reset to force new fetch
  }

  // Fetch logs for the newly selected build
  updateLogs();
}

function updateLogTitle(buildId) {
  const buildTitleElement = document.querySelector('.innerbody-header span');
  if (!buildTitleElement) return;

  // Update the log window status icon based on current view
  const logStatusIcon = document.querySelector('.innerbody-header .material-icons, .innerbody-header .loader');

  if (buildId && buildId !== 'all' && buildsData.length > 0) {
    const selectedBuild = buildsData.find(b => b.id === buildId);
    if (selectedBuild) {
      // Clean up build name like we do in sidebar
      let rawBuildName = selectedBuild.evaluation_target || selectedBuild.name || 'Build';
      let buildName = rawBuildName;
      if (typeof rawBuildName === 'string') {
        if (rawBuildName.startsWith('/nix/store/')) {
          const parts = rawBuildName.split('/').pop();
          if (parts) {
            buildName = parts.replace(/^[a-z0-9]{32}-/, '').replace(/\.drv$/, '');
          }
        } else if (rawBuildName.includes('-') && /^[a-z0-9]{32}-/.test(rawBuildName)) {
          buildName = rawBuildName.replace(/^[a-z0-9]{32}-/, '').replace(/\.drv$/, '');
        }
      }

      const architecture = selectedBuild.architecture || 'x86_64-linux';
      buildTitleElement.textContent = `${buildName} (${architecture})`;

      // Update log window status icon based on individual build status
      if (logStatusIcon) {
        const buildStatus = selectedBuild.status?.toLowerCase();
        if (buildStatus === 'completed') {
          logStatusIcon.className = 'material-icons green';
          logStatusIcon.textContent = 'check_circle';
        } else if (buildStatus === 'failed' || buildStatus === 'aborted') {
          logStatusIcon.className = 'material-icons red';
          logStatusIcon.textContent = 'cancel';
        } else if (buildStatus === 'building' || buildStatus === 'running') {
          logStatusIcon.className = 'loader';
          logStatusIcon.textContent = '';
        } else {
          logStatusIcon.className = 'loader';
          logStatusIcon.textContent = '';
        }
      }
    }
  } else {
    buildTitleElement.textContent = 'All Builds';

    // Update log window status icon based on evaluation status for "All Builds" view
    if (logStatusIcon && currentEvaluation) {
      const evalStatus = currentEvaluation.status;
      if (evalStatus === 'Completed') {
        logStatusIcon.className = 'material-icons green';
        logStatusIcon.textContent = 'check_circle';
      } else if (evalStatus === 'Failed' || evalStatus === 'Aborted') {
        logStatusIcon.className = 'material-icons red';
        logStatusIcon.textContent = 'cancel';
      } else {
        logStatusIcon.className = 'loader';
        logStatusIcon.textContent = '';
      }
    }
  }
}

function filterLogsByBuild(buildId) {
  const logContainer = document.querySelector('.details-content');
  if (!logContainer) return;

  const allLogLines = logContainer.querySelectorAll('.line');

  allLogLines.forEach(line => {
    const lineBuildId = line.getAttribute('data-build-id');
    // Show logs for the selected build only
    if (!lineBuildId || lineBuildId === buildId) {
      line.style.display = '';
    } else {
      line.style.display = 'none';
    }
  });

  // Scroll to bottom after filtering
  logContainer.scrollTop = logContainer.scrollHeight;
}

async function updateLogs() {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) {
    console.warn('Log container not found');
    return;
  }

  // Skip log updates if evaluation error is being displayed
  if (logContainer.querySelector('.evaluation-error')) {
    console.log('Skipping log update due to evaluation error');
    return;
  }

  // Only fetch logs for the currently selected build, or first build if none selected
  let targetBuildId = currentBuildId;
  if (!targetBuildId && buildIds.length > 0) {
    targetBuildId = buildIds[0];
    currentBuildId = targetBuildId;
  }

  if (!targetBuildId) {
    console.log('No build selected or available for log fetching');
    return;
  }

  // If build changed or this is the first time, stop existing stream and fetch initial logs
  if (!initialLogsFetched || streamingBuildId !== targetBuildId) {
    await stopLogStream();
    await fetchInitialLogs(targetBuildId);
    await startLogStream(targetBuildId);
  }
}

async function stopLogStream() {
  if (activeStreamReader) {
    console.log('Stopping existing log stream');
    try {
      await activeStreamReader.cancel();
    } catch (e) {
      console.warn('Error canceling stream:', e);
    }
    activeStreamReader = null;
  }
  streamingBuildId = null;
}

async function fetchInitialLogs(targetBuildId) {
  console.log(`Fetching initial logs for build: ${targetBuildId}`);
  let allLogs = [];

  try {
    // Always GET past logs first (BaseResponse)
    const pastLogsResponse = await fetch(`${window.baseUrl}/api/v1/builds/${targetBuildId}/log`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });

    let logContent = '';
    if (pastLogsResponse.ok) {
      const data = await pastLogsResponse.json();
      if (!data.error) {
        logContent = data.message || '';
      }
    } else {
      console.error(`Build ${targetBuildId}: Failed to fetch log, status:`, pastLogsResponse.status);
    }

    if (logContent.trim()) {
      const lines = logContent.split('\n').filter(line => {
        const trimmed = line.trim();
        // Filter out empty strings, pure quotes, and JSON-encoded empty strings
        return trimmed &&
               trimmed !== '""' &&
               trimmed !== "''" &&
               trimmed !== '"' &&
               trimmed !== "'" &&
               !(trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length <= 2);
      }).flatMap(line => {
        // Only remove first and last char if they are quotation marks
        if (line.length >= 2 &&
            ((line.startsWith('"') && line.endsWith('"')) ||
             (line.startsWith("'") && line.endsWith("'")))) {
          line = line.slice(1, -1);
        }

        // Filter out empty content after removing quotes
        if (!line.trim()) {
          return [];
        }

        // Process escaped characters (like \n, \t, etc.) and Unicode escape sequences
        line = line
          .replace(/\\t/g, '\t')
          .replace(/\\r/g, '\r')
          .replace(/\\"/g, '"')
          .replace(/\\'/g, "'")
          .replace(/\\u([0-9a-fA-F]{4})/g, (match, hex) => String.fromCharCode(parseInt(hex, 16)))
          .replace(/\\\\/g, '\\')
          .replace(/\\n/g, '\n'); // Process \n last to split into multiple lines

        // Split on actual newlines and process each line
        return line.split('\n').map(subLine => convertAnsiToHtml(subLine)).filter(subLine => subLine.trim() !== '');
      });

      console.log(`Build ${targetBuildId}: Found ${lines.length} log lines`);
      allLogs = lines.map(line => ({
        content: line,
        buildId: targetBuildId,
        timestamp: new Date().toISOString()
      }));
    } else {
      console.log(`Build ${targetBuildId}: No log data available`);
    }
  } catch (error) {
    console.error(`Error fetching initial logs for build ${targetBuildId}:`, error);
  }

  // Display initial logs
  displayLogs(allLogs, true);
  initialLogsFetched = true;
}

async function startLogStream(targetBuildId) {
  // Check if build is currently building
  const buildInfo = buildsData.find(build => build.id === targetBuildId);
  if (!buildInfo) {
    console.log(`Build ${targetBuildId} not found in buildsData`);
    return;
  }

  const status = buildInfo.status?.toLowerCase();
  const isBuilding = status === 'building' || status === 'running' || status === 'queued';

  if (!isBuilding) {
    console.log(`Build ${targetBuildId} is not building (status: ${buildInfo.status}), skipping stream`);
    return;
  }

  console.log(`Starting log stream for build ${targetBuildId}`);
  streamingBuildId = targetBuildId;

  try {
    const streamResponse = await fetch(`${window.baseUrl}/api/v1/builds/${targetBuildId}/log`, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/jsonstream",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });

    if (streamResponse.ok && streamResponse.body) {
      activeStreamReader = streamResponse.body.getReader();
      const decoder = new TextDecoder();

      try {
        while (true) {
          const { done, value } = await activeStreamReader.read();
          if (done) break;

          const chunk = decoder.decode(value, { stream: true });
          if (chunk.trim()) {
            processStreamChunk(chunk, targetBuildId);
          }
        }
      } catch (error) {
        if (error.name !== 'AbortError') {
          console.error(`Stream reading error for build ${targetBuildId}:`, error);
        }
      } finally {
        activeStreamReader = null;
        streamingBuildId = null;
      }
    } else if (!streamResponse.ok) {
      console.warn(`Build ${targetBuildId}: Stream response not ok, status:`, streamResponse.status);
    } else {
      console.warn(`Build ${targetBuildId}: Stream response body is null`);
    }
  } catch (streamError) {
    console.error(`Build ${targetBuildId}: Error starting stream:`, streamError);
    activeStreamReader = null;
    streamingBuildId = null;
  }
}

function processStreamChunk(chunk, targetBuildId) {
  // Process the streaming chunk and add new log lines
  const lines = chunk.split('\n').filter(line => {
    const trimmed = line.trim();
    return trimmed &&
           trimmed !== '""' &&
           trimmed !== "''" &&
           trimmed !== '"' &&
           trimmed !== "'" &&
           !(trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length <= 2);
  }).flatMap(line => {
    // Only remove first and last char if they are quotation marks
    if (line.length >= 2 &&
        ((line.startsWith('"') && line.endsWith('"')) ||
         (line.startsWith("'") && line.endsWith("'")))) {
      line = line.slice(1, -1);
    }

    if (!line.trim()) {
      return [];
    }

    // Process escaped characters
    line = line
      .replace(/\\t/g, '\t')
      .replace(/\\r/g, '\r')
      .replace(/\\"/g, '"')
      .replace(/\\'/g, "'")
      .replace(/\\u([0-9a-fA-F]{4})/g, (match, hex) => String.fromCharCode(parseInt(hex, 16)))
      .replace(/\\\\/g, '\\')
      .replace(/\\n/g, '\n');

    return line.split('\n').map(subLine => convertAnsiToHtml(subLine)).filter(subLine => subLine.trim() !== '');
  });

  if (lines.length > 0) {
    const newLogs = lines.map(line => ({
      content: line,
      buildId: targetBuildId,
      timestamp: new Date().toISOString()
    }));

    displayLogs(newLogs, false);
  }
}

function displayLogs(logs, isInitialLoad) {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;

  if (isInitialLoad) {
    logContainer.innerHTML = '';
    lastLogLength = 0;
  }

  if (logs.length === 0 && isInitialLoad) {
    logContainer.innerHTML = '<div class="line" style="color: #666; font-style: italic;">No logs</div>';
    return;
  }

  // Check if user is near the bottom before adding content
  const isNearBottom = (logContainer.scrollTop + logContainer.clientHeight) >= (logContainer.scrollHeight - 50);

  logs.forEach(logEntry => {
    if (logEntry.content && logEntry.content.trim()) {
      const lineDiv = document.createElement('div');
      lineDiv.className = 'line';
      lineDiv.setAttribute('data-build-id', logEntry.buildId);

      // Parse ANSI colors and convert to HTML
      const parsedContent = parseAnsiColors(logEntry.content);
      lineDiv.innerHTML = parsedContent;

      logContainer.appendChild(lineDiv);
    }
  });

  lastLogLength += logs.length;

  // Only auto-scroll if user was near the bottom or if it's the initial load
  if (isNearBottom || isInitialLoad) {
    logContainer.scrollTop = logContainer.scrollHeight;
    hideScrollToBottomButton();
  } else {
    showScrollToBottomButton();
  }

  // Update line numbers
  updateLineNumbers();
}

function updateLineNumbers() {
  let lineCounter = 1;
  document.querySelectorAll('.details-content .line').forEach(line => {
    // Skip line numbering for evaluation errors
    if (!line.closest('.evaluation-error')) {
      line.setAttribute('data-line-number', lineCounter++);
    }
  });
}

function parseAnsiColors(text) {
  // ANSI color codes mapping
  const ansiColors = {
    '30': 'color: #000000', // black
    '31': 'color: #ff4444', // red
    '32': 'color: #44ff44', // green
    '33': 'color: #ffff44', // yellow
    '34': 'color: #4444ff', // blue
    '35': 'color: #ff44ff', // magenta
    '36': 'color: #44ffff', // cyan
    '37': 'color: #ffffff', // white
    '90': 'color: #666666', // bright black (gray)
    '91': 'color: #ff6666', // bright red
    '92': 'color: #66ff66', // bright green
    '93': 'color: #ffff66', // bright yellow
    '94': 'color: #6666ff', // bright blue
    '95': 'color: #ff66ff', // bright magenta
    '96': 'color: #66ffff', // bright cyan
    '97': 'color: #ffffff', // bright white
  };

  const ansiStyles = {
    '1': 'font-weight: bold',
    '3': 'font-style: italic',
    '4': 'text-decoration: underline',
    '22': 'font-weight: normal',
    '23': 'font-style: normal',
    '24': 'text-decoration: none',
  };

  // Replace ANSI escape sequences
  let result = text;

  // Handle reset sequences
  result = result.replace(/\x1b\[0?m/g, '</span>');

  // Handle style and color sequences
  result = result.replace(/\x1b\[([0-9;]+)m/g, (match, codes) => {
    const codeList = codes.split(';');
    const styles = [];

    codeList.forEach(code => {
      if (ansiColors[code]) {
        styles.push(ansiColors[code]);
      }
      if (ansiStyles[code]) {
        styles.push(ansiStyles[code]);
      }
    });

    if (styles.length > 0) {
      return `<span style="${styles.join('; ')}">`;
    }
    return '';
  });

  // Clean up any remaining ANSI sequences that we didn't handle
  result = result.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '');

  return result;
}

function createScrollToBottomButton() {
  if (scrollToBottomButton) return;

  scrollToBottomButton = document.createElement('button');
  scrollToBottomButton.innerHTML = `
    <span class="material-symbols-outlined">keyboard_arrow_down</span>
    New logs
  `;
  scrollToBottomButton.className = 'scroll-to-bottom-btn';
  scrollToBottomButton.style.cssText = `
    position: fixed;
    bottom: 2rem;
    right: 2rem;
    background: #007bff;
    color: white;
    border: none;
    border-radius: 24px;
    padding: 0.75rem 1rem;
    display: none;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
    font-size: 0.875rem;
    font-weight: 500;
    box-shadow: 0 4px 12px rgba(0, 123, 255, 0.3);
    z-index: 1000;
    transition: all 0.2s ease;
  `;

  scrollToBottomButton.addEventListener('click', () => {
    const logContainer = document.querySelector(".details-content");
    if (logContainer) {
      logContainer.scrollTop = logContainer.scrollHeight;
      hideScrollToBottomButton();
    }
  });

  scrollToBottomButton.addEventListener('mouseenter', () => {
    scrollToBottomButton.style.transform = 'translateY(-2px)';
    scrollToBottomButton.style.boxShadow = '0 6px 16px rgba(0, 123, 255, 0.4)';
  });

  scrollToBottomButton.addEventListener('mouseleave', () => {
    scrollToBottomButton.style.transform = 'translateY(0)';
    scrollToBottomButton.style.boxShadow = '0 4px 12px rgba(0, 123, 255, 0.3)';
  });

  document.body.appendChild(scrollToBottomButton);
}

function showScrollToBottomButton() {
  if (!scrollToBottomButton) createScrollToBottomButton();
  scrollToBottomButton.style.display = 'flex';
}

function hideScrollToBottomButton() {
  if (scrollToBottomButton) {
    scrollToBottomButton.style.display = 'none';
  }
}

function setupScrollListener() {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;

  logContainer.addEventListener('scroll', () => {
    const isNearBottom = (logContainer.scrollTop + logContainer.clientHeight) >= (logContainer.scrollHeight - 50);

    if (isNearBottom) {
      hideScrollToBottomButton();
    } else if (lastLogLength > 0) { // Only show if there are logs
      showScrollToBottomButton();
    }
  });
}

// Initialize the page
async function initializePage() {
  console.log('Initializing page...');
  console.log('baseUrl:', window.baseUrl);
  console.log('evaluationId:', evaluationId);
  console.log('window.token:', window.token ? 'present' : 'missing');

  // Set up scroll listener for auto-scroll behavior
  setupScrollListener();

  // Set up Build tab click handler to show all builds
  const buildTab = document.getElementById('build-tab');
  if (buildTab) {
    buildTab.addEventListener('click', () => {
      selectAllBuilds();
    });
  }

  // Check if there's an initial evaluation error from the server
  if (window.initialEvaluationError) {
    displayEvaluationError(window.initialEvaluationError);
    // Still set Build tab as active even with error
    selectAllBuilds();
    return; // Don't fetch builds/logs if there's an error
  }

  // Fetch initial evaluation data to get created_at time
  await checkBuildStatus();

  // Always fetch builds first
  await fetchBuilds();

  // Set Build tab as active by default if no builds are selected
  if (!currentBuildId) {
    selectAllBuilds();
  } else {
    // Always fetch logs initially, regardless of status
    await updateLogs();
  }
}

function selectAllBuilds() {
  // Clear current build selection
  currentBuildId = 'all';

  // Remove active state from all build items
  document.querySelectorAll('.build-item').forEach(item => {
    item.classList.remove('active');
  });

  // Add active state to build tab
  const buildTab = document.getElementById('build-tab');
  if (buildTab) {
    buildTab.classList.add('active');
  }

  // Update log title to show "All Builds"
  const buildTitleElement = document.querySelector('.innerbody-header span');
  if (buildTitleElement) {
    buildTitleElement.textContent = 'All Builds';
  }

  // Clear existing logs and fetch from all builds
  const logContainer = document.querySelector('.details-content');
  if (logContainer) {
    logContainer.innerHTML = '<div class="line" style="color: #666; font-style: italic;">Loading build logs...</div>';
    lastLogLength = 0; // Reset log counter
  }

  // Check if there's an evaluation error to display
  if (window.initialEvaluationError) {
    displayEvaluationError(window.initialEvaluationError);
    return;
  }

  // Fetch logs from all builds
  updateAllBuildsLogs();
}

async function updateAllBuildsLogs() {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;

  console.log(`Fetching logs for all builds in evaluation ${evaluationId}`);
  let allLogs = [];

  try {
    // Use post_evaluation_builds endpoint for aggregated log streaming
    const response = await fetch(`${window.baseUrl}/api/v1/evals/${evaluationId}/builds`, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/jsonstream",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });

    if (response.ok) {
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let logContent = '';

      try {
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          const chunk = decoder.decode(value, { stream: true });
          logContent += chunk;
        }
      } finally {
        reader.releaseLock();
      }

      if (logContent.trim()) {
        const lines = logContent.split('\n').filter(line => {
          const trimmed = line.trim();
          // Filter out empty strings, pure quotes, and JSON-encoded empty strings
          return trimmed &&
                 trimmed !== '""' &&
                 trimmed !== "''" &&
                 trimmed !== '"' &&
                 trimmed !== "'" &&
                 !(trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length <= 2);
        }).flatMap(line => {
          // Remove first and last char to remove quotation marks
          if (line.length >= 2) {
            line = line.slice(1, -1);
          }

          // Filter out empty content after removing quotes
          if (!line.trim()) {
            return [];
          }

          // Process escaped characters (like \n, \t, etc.) and Unicode escape sequences
          line = line
            .replace(/\\t/g, '\t')
            .replace(/\\r/g, '\r')
            .replace(/\\"/g, '"')
            .replace(/\\'/g, "'")
            .replace(/\\u([0-9a-fA-F]{4})/g, (match, hex) => String.fromCharCode(parseInt(hex, 16)))
            .replace(/\\\\/g, '\\')
            .replace(/\\n/g, '\n'); // Process \n last to split into multiple lines

          // Split on actual newlines and process each line
          return line.split('\n').map(subLine => convertAnsiToHtml(subLine)).filter(subLine => subLine.trim() !== '');
        });

        allLogs = lines.map(line => ({
          content: line,
          buildId: 'all',
          timestamp: new Date().toISOString()
        }));
      }
    }
  } catch (error) {
    console.error(`Error fetching logs for all builds:`, error);
  }

  // Display all logs
  allLogs.forEach(logEntry => {
    if (logEntry.content && logEntry.content.trim()) {
      const lineDiv = document.createElement('div');
      lineDiv.className = 'line';
      lineDiv.setAttribute('data-build-id', logEntry.buildId);
      const parsedContent = parseAnsiColors(logEntry.content);
      lineDiv.innerHTML = parsedContent;
      logContainer.appendChild(lineDiv);
    }
  });

  logContainer.scrollTop = logContainer.scrollHeight;
}

// Check if evaluation is in a running state and start polling accordingly
function shouldStartPolling() {
  const statusElements = document.querySelectorAll('.status-text');
  for (let element of statusElements) {
    const status = element.textContent.toLowerCase();
    if (status.includes('building') || status.includes('evaluating') || 
        status.includes('running') || status.includes('queued')) {
      return true;
    }
  }
  return false;
}

// Initialize page first
initializePage().then(() => {
  // After initialization, check if we should start polling
  if (shouldStartPolling()) {
    console.log('Starting live polling for evaluation updates');

    // Start status polling
    statusCheckInterval = setInterval(checkBuildStatus, 2000); // Check every 2 seconds

    // Start log polling for live updates
    logCheckInterval = setInterval(updateLogs, 3000); // Check logs every 3 seconds

    // Auto-stop polling after 30 minutes to prevent endless polling
    setTimeout(() => {
      if (statusCheckInterval) {
        clearInterval(statusCheckInterval);
      }
      if (logCheckInterval) {
        clearInterval(logCheckInterval);
      }
      if (durationUpdateInterval) {
        clearInterval(durationUpdateInterval);
        durationUpdateInterval = null;
      }
      console.log('Auto-stopped polling after 30 minutes');
    }, 30 * 60 * 1000);
  } else {
    console.log('Evaluation is completed, polling not needed');

    // For completed evaluations, ensure we have the latest status and logs
    checkBuildStatus().then(() => {
      // Force a final log fetch for completed builds
      if (buildIds.length > 0) {
        updateLogs();
      }
    });
  }
});
