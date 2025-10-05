let statusCheckInterval;
let logCheckInterval;
let buildCompleted = false;
let lastLogLength = 0;
let buildIds = [];

// Get variables from global scope
const baseUrl = window.location.origin;
const evaluationId = window.location.pathname.split('/').pop();
const token = window.token || '';

async function checkBuildStatus() {
  try {
    const response = await fetch(`${baseUrl}/api/v1/evals/${evaluationId}/status`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${token}`,
        "Content-Type": "application/jsonstream",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        const evaluation = data.message;
        updateBuildStatus(evaluation.status);
        
        // Display evaluation error if it exists
        displayEvaluationError(evaluation.error);
        
        // Get builds for this evaluation
        await fetchBuilds();
        
        // Stop polling if build is completed
        if (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted') {
          clearInterval(statusCheckInterval);
          clearInterval(logCheckInterval);
          buildCompleted = true;
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
        "Authorization": `Bearer ${token}`,
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
    const response = await fetch(`${baseUrl}/api/v1/evals/${evaluationId}/builds`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${token}`,
        "Content-Type": "application/jsonstream",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        buildIds = data.message.map(build => build.id);
        await updateLogs();
      }
    }
  } catch (error) {
    console.error("Error fetching builds:", error);
  }
}

async function updateLogs() {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;

  // Skip log updates if evaluation error is being displayed
  if (logContainer.querySelector('.evaluation-error')) {
    return;
  }

  let allLogs = [];

  for (const buildId of buildIds) {
    try {
      const response = await fetch(`${baseUrl}/api/v1/builds/${buildId}`, {
        method: "GET",
        credentials: "include",
        withCredentials: true,
        mode: "cors",
        headers: {
          "Authorization": `Bearer ${token}`,
          "Content-Type": "application/jsonstream",
          "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
        },
      });

      if (response.ok) {
        const data = await response.json();
        if (!data.error && data.message.log) {
          const lines = data.message.log.split('\n');
          allLogs = allLogs.concat(lines.map(line => ({
            content: line,
            buildId: buildId,
            timestamp: data.message.created_at || new Date().toISOString()
          })));
        }
      }
    } catch (error) {
      console.error(`Error fetching build ${buildId}:`, error);
    }
  }

  // Only update if we have new content
  if (allLogs.length > lastLogLength) {
    const newLines = allLogs.slice(lastLogLength);
    newLines.forEach(logEntry => {
      if (logEntry.content && logEntry.content.trim()) {
        const lineDiv = document.createElement('div');
        lineDiv.className = 'line';
        lineDiv.setAttribute('data-build-id', logEntry.buildId);
        lineDiv.textContent = logEntry.content;
        logContainer.appendChild(lineDiv);
      }
    });
    lastLogLength = allLogs.length;

    // Auto-scroll to bottom for live updates
    logContainer.scrollTop = logContainer.scrollHeight;

    // Update line numbers
    updateLineNumbers();
  }
}

function updateLineNumbers() {
  let lineCounter = 1;
  document.querySelectorAll('.details-content .line').forEach(line => {
    line.setAttribute('data-line-number', lineCounter++);
  });
}

// Initialize the page
async function initializePage() {
  // Check if there's an initial evaluation error from the server
  if (window.initialEvaluationError) {
    displayEvaluationError(window.initialEvaluationError);
    return; // Don't fetch builds/logs if there's an error
  }
  
  await fetchBuilds();
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
initializePage();

// Only start polling if evaluation is in a running state
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
  console.log('Evaluation is not running, live polling not started');
}
