let statusCheckInterval;
let buildCompleted = false;

// Get variables from global scope
const baseUrl = window.location.origin;
const evaluationId = window.location.pathname.split('/').pop();

async function checkBuildStatus() {
  try {
    const response = await fetch(`${baseUrl}/evals/${evaluationId}`, {
      method: "GET",
      credentials: "include",
      headers: {
        "Authorization": `Bearer ${token}`,
        "Content-Type": "application/json",
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        const evaluation = data.message;
        updateBuildStatus(evaluation.status);
        
        // Stop polling if build is completed
        if (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted') {
          clearInterval(statusCheckInterval);
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
  
  // Show/hide abort button
  if (abortButton) {
    if (status === 'Building' || status === 'Evaluating' || status === 'Queued') {
      abortButton.style.display = 'inline-block';
    } else {
      abortButton.style.display = 'none';
    }
  }
}

async function abortBuild() {
  if (confirm('Are you sure you want to abort this build? This action cannot be undone.')) {
    try {
      const response = await fetch(`${baseUrl}/evals/${evaluationId}`, {
        method: "POST",
        credentials: "include",
        headers: {
          "Authorization": `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ method: 'abort' })
      });
      
      if (response.ok) {
        const data = await response.json();
        if (!data.error) {
          // Force a status check to update UI
          await checkBuildStatus();
        } else {
          alert('Failed to abort build: ' + data.message);
        }
      } else {
        alert('Failed to abort build');
      }
    } catch (error) {
      console.error("Error aborting build:", error);
      alert('Error aborting build');
    }
  }
}

async function makeRequest() {
  try {
    fetch(url, {
      method: "POST",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${token}`,
        "Content-Type": "application/jsonstream",
      },
    }).then(async (response) => {
      const reader = response.body.getReader();
      const logContainer = document.querySelector(".details-content");

      while (true) {
        const { done, value } = await reader.read();
        const text = new TextDecoder("utf-8").decode(value);

        if (done) {
          logContainer.innerHTML += `<div class="line">End of Log</div>`;
          // Final status check when log stream ends
          await checkBuildStatus();
          break;
        }

        if (text) {
          try {
            const data = JSON.parse(text);

            if (data.hasOwnProperty("error")) {
              console.error(data["message"]);
            } else {
              logContainer.innerHTML += `<div class="line">${data.message}</div>`;
              logContainer.scrollTop = logContainer.scrollHeight;
            }
          } catch (err) {
            console.error("JSON parsing error:", err);
          }
        }
      }
    });
  } catch (error) {
    console.error("Error during fetch:", error);
  }
}

// Start status polling
statusCheckInterval = setInterval(checkBuildStatus, 2000); // Check every 2 seconds

// Start log streaming
makeRequest();
