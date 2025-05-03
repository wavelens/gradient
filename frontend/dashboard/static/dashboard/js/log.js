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
          logContainer.innerHTML += `<div class="line">Log beendet</div>`;
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
            console.error("Fehler beim Parsen von JSON:", err);
          }
        }
      }
    });
  } catch (error) {
    console.error("Error during fetch:", error);
  }
}

makeRequest();
