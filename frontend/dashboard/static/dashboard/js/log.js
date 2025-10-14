/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let statusCheckInterval;
let logCheckInterval;
let buildCompleted = false;
let lastLogLength = 0;
let buildIds = [];
let scrollToBottomButton = null;
let buildsData = [];
let currentBuildId = null;

// Get variables from global scope
const baseUrl = window.location.origin;
const evaluationId = window.location.pathname.split('/').pop();

async function checkBuildStatus() {
  try {
    const response = await fetch(`${baseUrl}/api/v1/evals/${evaluationId}`, {
      method: "GET",
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
      const data = await response.json();
      if (!data.error) {
        const evaluation = data.message;
        updateBuildStatus(evaluation.status);

        // Update duration - use updated_at for completed builds if available
        const endTime = (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted')
          ? evaluation.updated_at || evaluation.created_at
          : evaluation.created_at;
        updateDuration(evaluation.created_at, evaluation.status, endTime);

        // Display evaluation error if it exists
        displayEvaluationError(evaluation.error);

        // Get builds for this evaluation (this will also update the sidebar)
        await fetchBuilds();

        // Stop polling if build is completed, but fetch logs one final time
        if (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted') {
          clearInterval(statusCheckInterval);
          clearInterval(logCheckInterval);
          buildCompleted = true;

          // Fetch final logs when build is complete
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
    const response = await fetch(`${baseUrl}/api/v1/evals/${evaluationId}/abort`, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/jsonstream",
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
    const response = await fetch(`${baseUrl}/api/v1/evals/${evaluationId}/builds`, {
      method: "GET",
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

  // Remove loading placeholder
  const loadingItem = document.getElementById('loading-builds');
  if (loadingItem) {
    loadingItem.remove();
  }

  // Set up "All Builds" click handler if not already done
  const allBuildsItem = document.getElementById('all-builds-item');
  if (allBuildsItem && !allBuildsItem.hasAttribute('data-handler-added')) {
    allBuildsItem.addEventListener('click', () => {
      selectBuild('all');
    });
    allBuildsItem.setAttribute('data-handler-added', 'true');
  }

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

  // Remove any existing build items (but keep the "All Builds" item)
  const existingBuilds = buildsContainer.querySelectorAll('.build-item:not(#all-builds-item):not(#loading-builds)');
  existingBuilds.forEach(item => item.remove());

  // Add individual build items
  buildsData.forEach((build, index) => {
    const buildItem = createBuildItem(build, index);
    buildsContainer.appendChild(buildItem);
  });

  // Update the "All Builds" summary
  updateAllBuildsSummary();
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
      statusIconHtml = '<span class="material-icons build-status-icon pending-color">schedule</span>';
      statusText = 'Queued';
      break;
  }

  // Format build name - use evaluation target or fallback
  const buildName = build.evaluation_target || build.name || `Build ${index + 1}`;

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

function updateAllBuildsSummary() {
  const allBuildsItem = document.getElementById('all-builds-item');
  if (!allBuildsItem || buildsData.length === 0) return;

  const total = buildsData.length;
  const completed = buildsData.filter(b => b.status?.toLowerCase() === 'completed').length;
  const failed = buildsData.filter(b => b.status?.toLowerCase() === 'failed').length;
  const running = buildsData.filter(b =>
    ['running', 'building'].includes(b.status?.toLowerCase())).length;

  const summaryText = running > 0
    ? `${running} running, ${completed} done`
    : `${total} builds: ${completed} ✅ ${failed} ❌`;

  const detailsSpan = allBuildsItem.querySelector('.build-details span');
  if (detailsSpan) {
    detailsSpan.textContent = summaryText;
  }
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

  // Filter logs to show only this build
  filterLogsByBuild(buildId);
}

function filterLogsByBuild(buildId) {
  const logContainer = document.querySelector('.details-content');
  if (!logContainer) return;

  const allLogLines = logContainer.querySelectorAll('.line');

  allLogLines.forEach(line => {
    const lineBuildId = line.getAttribute('data-build-id');
    if (buildId === 'all' || !lineBuildId || lineBuildId === buildId) {
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

  console.log(`Fetching logs for ${buildIds.length} builds:`, buildIds);
  let allLogs = [];

  for (const buildId of buildIds) {
    try {
      const response = await fetch(`${baseUrl}/api/v1/builds/${buildId}`, {
        method: "GET",
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
        const data = await response.json();
        if (!data.error && data.message.log) {
          const lines = data.message.log.split('\n');
          console.log(`Build ${buildId}: Found ${lines.length} log lines`);
          allLogs = allLogs.concat(lines.map(line => ({
            content: line,
            buildId: buildId,
            timestamp: data.message.created_at || new Date().toISOString()
          })));
        } else {
          console.log(`Build ${buildId}: No log data available`, data);
        }
      } else {
        console.error(`Build ${buildId}: Failed to fetch log, status:`, response.status);
      }
    } catch (error) {
      console.error(`Error fetching build ${buildId}:`, error);
    }
  }

  console.log(`Total logs collected: ${allLogs.length}, lastLogLength: ${lastLogLength}`);

  // Update if we have new content OR if this is the first load for completed builds
  const isFirstLoad = lastLogLength === 0;
  const hasNewContent = allLogs.length > lastLogLength;

  if (hasNewContent || (isFirstLoad && allLogs.length > 0)) {
    // On first load, clear any existing content to prevent duplication
    if (isFirstLoad) {
      logContainer.innerHTML = '';
    }

    // Check if user is near the bottom before adding content
    const isNearBottom = (logContainer.scrollTop + logContainer.clientHeight) >= (logContainer.scrollHeight - 50);

    // For first load of completed builds, show all logs; otherwise show only new logs
    const linesToShow = isFirstLoad ? allLogs : allLogs.slice(lastLogLength);

    linesToShow.forEach(logEntry => {
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
    lastLogLength = allLogs.length;

    // Only auto-scroll if user was near the bottom (within 50px) or if it's the first load
    if (isNearBottom || isFirstLoad) {
      logContainer.scrollTop = logContainer.scrollHeight;
      hideScrollToBottomButton();
    } else {
      showScrollToBottomButton();
    }

    // Update line numbers
    updateLineNumbers();
  }
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
  // Set up scroll listener for auto-scroll behavior
  setupScrollListener();

  // Check if there's an initial evaluation error from the server
  if (window.initialEvaluationError) {
    displayEvaluationError(window.initialEvaluationError);
    return; // Don't fetch builds/logs if there's an error
  }

  // Always fetch builds first
  await fetchBuilds();

  // Always fetch logs initially, regardless of status
  await updateLogs();
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
